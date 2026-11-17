//! Tool, resource and prompt results → model-ready parts (06 §7 "Results"), marked with
//! their taint source (06 §5.5 SA-29, 08 §2.2).
//!
//! Structured content first; JSON-looking text parsed; binary (images, audio, blob
//! resources) decoded for the caller to store as a blob with a path note (D24). Central
//! truncation (06 §2.4) happens after this, in the tool framework.

use base64::Engine;
use serde_json::Value;

/// Where untrusted content came from. MCP output is untrusted unless the server is on
/// the org/user allowlist of trusted servers (SA-29); `trusted` records that decision so
/// the turn loop can taint the turn ("This step follows content from <label>").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintSource {
    pub server: String,
    pub trusted: bool,
}

impl TaintSource {
    /// Whether this content taints the turn.
    pub fn taints(&self) -> bool {
        !self.trusted
    }

    /// Text for the ask card.
    pub fn label(&self) -> String {
        format!("MCP server \"{}\"", self.server)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Structured content or JSON parsed from a text item.
    Json(Value),
    Text(String),
    /// Decoded image/audio/blob; the caller stores it and tells the model the path.
    Binary {
        mime: String,
        data: Vec<u8>,
        uri: Option<String>,
    },
    /// An embedded text resource.
    Resource {
        uri: String,
        mime: Option<String>,
        text: String,
    },
    /// A link the model can read with the resource tools.
    ResourceLink {
        uri: String,
        name: Option<String>,
        mime: Option<String>,
    },
}

/// A converted `tools/call` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub parts: Vec<Part>,
    /// `isError: true`: the tool ran and failed; shown to the model as an error result.
    pub is_error: bool,
    pub taint: TaintSource,
}

impl ToolResult {
    /// Plain-text rendering (JSON pretty-printed, binaries as placeholders).
    pub fn to_text(&self) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
                Part::Text(t) => t.clone(),
                Part::Binary { mime, data, uri } => format!(
                    "[binary {mime}, {} bytes{}]",
                    data.len(),
                    uri.as_ref().map(|u| format!(", {u}")).unwrap_or_default()
                ),
                Part::Resource { uri, text, .. } => format!("[resource {uri}]\n{text}"),
                Part::ResourceLink { uri, name, .. } => format!(
                    "[resource link {uri}{}]",
                    name.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
                ),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Convert a raw `CallToolResult`.
pub fn tool_result(raw: &Value, taint: TaintSource) -> ToolResult {
    let mut parts = Vec::new();
    let structured = raw
        .get("structuredContent")
        .filter(|v| !v.is_null())
        .cloned();
    if let Some(s) = &structured {
        parts.push(Part::Json(s.clone()));
    }
    for item in raw
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let part = content_part(item);
        // Servers repeat structured content as serialized text for old clients: skip it.
        if let (Some(s), Part::Json(j)) = (&structured, &part)
            && s == j
        {
            continue;
        }
        parts.push(part);
    }
    ToolResult {
        parts,
        is_error: raw.get("isError").and_then(Value::as_bool).unwrap_or(false),
        taint,
    }
}

/// Convert one content block (tool results, prompt messages).
pub fn content_part(item: &Value) -> Part {
    let s = |k: &str| item.get(k).and_then(Value::as_str).map(String::from);
    match item.get("type").and_then(Value::as_str) {
        Some("text") => text_part(&s("text").unwrap_or_default()),
        Some("image") | Some("audio") => binary(
            s("data").as_deref(),
            s("mimeType").unwrap_or_else(|| "application/octet-stream".into()),
            None,
        ),
        Some("resource") => item
            .get("resource")
            .map(resource_part)
            .unwrap_or_else(|| Part::Text(String::new())),
        Some("resource_link") => Part::ResourceLink {
            uri: s("uri").unwrap_or_default(),
            name: s("name"),
            mime: s("mimeType"),
        },
        _ => Part::Json(item.clone()),
    }
}

/// Convert one `ResourceContents` (text or blob).
pub fn resource_part(r: &Value) -> Part {
    let uri = r
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mime = r.get("mimeType").and_then(Value::as_str).map(String::from);
    if let Some(text) = r.get("text").and_then(Value::as_str) {
        Part::Resource {
            uri,
            mime,
            text: text.to_string(),
        }
    } else {
        binary(
            r.get("blob").and_then(Value::as_str),
            mime.unwrap_or_else(|| "application/octet-stream".into()),
            Some(uri),
        )
    }
}

/// `resources/read` → parts.
pub fn resource_contents(raw: &Value) -> Vec<Part> {
    raw.get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(resource_part)
        .collect()
}

fn text_part(t: &str) -> Part {
    let trimmed = t.trim();
    if ((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && let Ok(v) = serde_json::from_str::<Value>(trimmed)
    {
        return Part::Json(v);
    }
    Part::Text(t.to_string())
}

fn binary(data: Option<&str>, mime: String, uri: Option<String>) -> Part {
    match data.map(|d| base64::engine::general_purpose::STANDARD.decode(d)) {
        Some(Ok(bytes)) => Part::Binary {
            mime,
            data: bytes,
            uri,
        },
        _ => Part::Text(format!("[undecodable {mime} content]")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t() -> TaintSource {
        TaintSource {
            server: "s".into(),
            trusted: false,
        }
    }

    #[test]
    fn structured_first_and_dedup() {
        let r = tool_result(
            &json!({
                "content": [
                    {"type": "text", "text": "{\"a\":1}"},
                    {"type": "text", "text": "[1,2]"},
                    {"type": "text", "text": "plain {not json"},
                    {"type": "image", "data": "aGk=", "mimeType": "image/png"},
                    {"type": "resource", "resource": {"uri": "file:///x", "text": "body"}},
                    {"type": "resource_link", "uri": "file:///y", "name": "y"}
                ],
                "structuredContent": {"a": 1},
                "isError": true
            }),
            t(),
        );
        assert!(r.is_error);
        assert!(r.taint.taints());
        assert_eq!(r.parts[0], Part::Json(json!({"a": 1})));
        assert_eq!(r.parts[1], Part::Json(json!([1, 2])));
        assert!(matches!(r.parts[2], Part::Text(_)));
        assert_eq!(
            r.parts[3],
            Part::Binary {
                mime: "image/png".into(),
                data: b"hi".to_vec(),
                uri: None
            }
        );
        assert!(matches!(r.parts[4], Part::Resource { .. }));
        assert!(matches!(r.parts[5], Part::ResourceLink { .. }));
        assert_eq!(r.parts.len(), 6);
        assert!(r.to_text().contains("[binary image/png, 2 bytes]"));
    }

    #[test]
    fn resources_text_and_blob() {
        let parts = resource_contents(&json!({"contents": [
            {"uri": "a", "text": "x", "mimeType": "text/plain"},
            {"uri": "b", "blob": "aGk=", "mimeType": "application/pdf"},
            {"uri": "c", "blob": "!!!"}
        ]}));
        assert!(matches!(&parts[0], Part::Resource { text, .. } if text == "x"));
        assert!(matches!(&parts[1], Part::Binary { uri: Some(u), .. } if u == "b"));
        assert!(matches!(&parts[2], Part::Text(_)));
    }
}
