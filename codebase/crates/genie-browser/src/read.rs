//! `browser_read`: the page's main readable text as Markdown (≤ 8k tokens, 06 §10).
//!
//! Built from the same stripped accessibility outline as the snapshot (so hidden text
//! never appears), scoped to the first `main` / `article` landmark when there is one
//! (readability-style), or to a ref subtree.

use crate::snapshot::{SnapNode, Snapshot};

const BLOCKS: &[&str] = &[
    "paragraph",
    "heading",
    "listitem",
    "row",
    "blockquote",
    "list",
    "table",
    "article",
    "main",
    "region",
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "form",
    "figure",
    "group",
    "code",
    "term",
    "definition",
    "separator",
];

const SKIP: &[&str] = &[
    "button",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "slider",
    "spinbutton",
    "switch",
    "listbox",
    "option",
    "menuitem",
];

struct Md {
    out: String,
    buf: String,
    block_depth: Option<usize>,
    prefix: &'static str,
}

impl Md {
    fn flush(&mut self) {
        let t = self.buf.trim();
        if !t.is_empty() {
            if !self.out.is_empty() {
                self.out.push_str("\n\n");
            }
            self.out.push_str(self.prefix);
            self.out.push_str(t);
        }
        self.buf.clear();
        self.block_depth = None;
        self.prefix = "";
    }

    fn inline(&mut self, s: &str) {
        if !self.buf.is_empty() && !self.buf.ends_with(' ') {
            self.buf.push(' ');
        }
        self.buf.push_str(s);
    }
}

fn level(n: &SnapNode) -> usize {
    n.flags
        .iter()
        .find_map(|f| f.strip_prefix("level=")?.parse().ok())
        .unwrap_or(2)
        .clamp(1, 6)
}

/// Markdown of the page (or the subtree at `scope`), cut to `max_chars`.
pub fn markdown(s: &Snapshot, scope: Option<&str>, max_chars: usize) -> Result<String, String> {
    let start = match scope {
        Some(r) => Some(
            s.nodes
                .iter()
                .position(|n| n.r#ref.as_deref() == Some(r))
                .ok_or_else(|| format!("unknown ref `{r}`; take a new snapshot"))?,
        ),
        None => s
            .nodes
            .iter()
            .position(|n| n.role == "main" || n.role == "article"),
    };
    let range: Vec<usize> = match start {
        Some(i) => {
            let d = s.nodes[i].depth;
            std::iter::once(i)
                .chain((i + 1..s.nodes.len()).take_while(|j| s.nodes[*j].depth > d))
                .collect()
        }
        None => (0..s.nodes.len()).collect(),
    };
    let mut md = Md {
        out: String::new(),
        buf: String::new(),
        block_depth: None,
        prefix: "",
    };
    let mut skip_below: Option<usize> = None;
    for i in range {
        let n = &s.nodes[i];
        if let Some(d) = skip_below {
            if n.depth > d {
                continue;
            }
            skip_below = None;
        }
        if md.block_depth.is_some_and(|d| n.depth <= d) {
            md.flush();
        }
        let role = n.role.as_str();
        if SKIP.contains(&role) {
            skip_below = Some(n.depth);
            continue;
        }
        match role {
            "text" => md.inline(&n.name),
            "link" => {
                match &n.url {
                    Some(u) if !n.name.is_empty() => md.inline(&format!("[{}]({u})", n.name)),
                    _ if !n.name.is_empty() => md.inline(&n.name),
                    _ => {}
                }
                // The link's own text children repeat its name.
                skip_below = Some(n.depth);
            }
            "image" | "img" => {
                if !n.name.is_empty() {
                    md.inline(&format!("![{}]", n.name));
                }
            }
            "heading" => {
                md.flush();
                md.prefix = match level(n) {
                    1 => "# ",
                    2 => "## ",
                    3 => "### ",
                    4 => "#### ",
                    5 => "##### ",
                    _ => "###### ",
                };
                md.inline(&n.name);
                md.block_depth = Some(n.depth);
                skip_below = Some(n.depth);
            }
            "cell" | "gridcell" | "columnheader" | "rowheader" => {
                if !md.buf.is_empty() {
                    md.buf.push_str(" |");
                }
                if !n.name.is_empty() {
                    md.inline(&n.name);
                    skip_below = Some(n.depth);
                }
            }
            r if BLOCKS.contains(&r) => {
                md.flush();
                md.prefix = match r {
                    "listitem" => "- ",
                    "blockquote" => "> ",
                    _ => "",
                };
                md.block_depth = Some(n.depth);
            }
            _ => {}
        }
    }
    md.flush();
    let mut out = md.out;
    if out.len() > max_chars {
        let mut cut = max_chars.saturating_sub(60);
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n\n… (cut; use `scope` with a ref to read a part)");
    }
    if out.is_empty() {
        out.push_str("(no readable text)");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{PageMeta, RefTable};
    use serde_json::json;

    #[test]
    fn article_to_markdown() {
        let ax = json!({"nodes": [
            {"nodeId": "1", "role": {"value": "RootWebArea"}, "childIds": ["2", "3"]},
            {"nodeId": "2", "role": {"value": "navigation"}, "childIds": ["21"], "backendDOMNodeId": 2},
            {"nodeId": "21", "role": {"value": "link"}, "name": {"value": "Home"}, "backendDOMNodeId": 21},
            {"nodeId": "3", "role": {"value": "main"}, "childIds": ["4", "5", "7", "9"], "backendDOMNodeId": 3},
            {"nodeId": "4", "role": {"value": "heading"}, "name": {"value": "Release notes"}, "properties": [{"name": "level", "value": {"value": 1}}], "backendDOMNodeId": 4},
            {"nodeId": "5", "role": {"value": "paragraph"}, "childIds": ["51", "52", "53"], "backendDOMNodeId": 5},
            {"nodeId": "51", "role": {"value": "StaticText"}, "name": {"value": "See the"}},
            {"nodeId": "52", "role": {"value": "link"}, "name": {"value": "docs"}, "childIds": ["521"], "properties": [{"name": "url", "value": {"value": "https://x.test/docs"}}], "backendDOMNodeId": 52},
            {"nodeId": "521", "role": {"value": "StaticText"}, "name": {"value": "docs"}},
            {"nodeId": "53", "role": {"value": "StaticText"}, "name": {"value": "for details."}},
            {"nodeId": "7", "role": {"value": "list"}, "childIds": ["71", "72"], "backendDOMNodeId": 7},
            {"nodeId": "71", "role": {"value": "listitem"}, "childIds": ["711"], "backendDOMNodeId": 71},
            {"nodeId": "711", "role": {"value": "StaticText"}, "name": {"value": "Faster"}},
            {"nodeId": "72", "role": {"value": "listitem"}, "childIds": ["721"], "backendDOMNodeId": 72},
            {"nodeId": "721", "role": {"value": "StaticText"}, "name": {"value": "Smaller"}},
            {"nodeId": "9", "role": {"value": "button"}, "name": {"value": "Subscribe"}, "backendDOMNodeId": 9}
        ]});
        let s = Snapshot::build(
            &ax,
            &json!({}),
            PageMeta::default(),
            &mut RefTable::default(),
        );
        let md = markdown(&s, None, 10_000).unwrap();
        assert_eq!(
            md,
            "# Release notes\n\nSee the [docs](https://x.test/docs) for details.\n\n- Faster\n\n- Smaller"
        );
        let cut = markdown(&s, None, 70).unwrap();
        assert!(cut.contains("(cut;"));
        assert!(markdown(&s, Some("e404"), 100).is_err());
    }
}
