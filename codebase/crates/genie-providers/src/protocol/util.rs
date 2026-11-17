//! Shared helpers for the adapters.

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::compile::{CompiledRequest, WireMessage};
use crate::error::ProviderError;
use crate::model::{Message, WireRecord};

/// A request body: `head` fields, then the history list under `list_key`, then `tail`.
/// History items are either fresh JSON or stored raw JSON replayed byte-for-byte (D18).
#[derive(Debug, Default)]
pub struct WireBody {
    pub head: Map<String, Value>,
    pub list_key: &'static str,
    pub list: Vec<WireItem>,
    pub tail: Map<String, Value>,
}

#[derive(Debug)]
pub enum WireItem {
    Raw(Box<RawValue>),
    Json(Value),
}

impl Serialize for WireItem {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            WireItem::Raw(r) => r.serialize(s),
            WireItem::Json(v) => v.serialize(s),
        }
    }
}

impl Serialize for WireBody {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        for (k, v) in &self.head {
            m.serialize_entry(k, v)?;
        }
        m.serialize_entry(self.list_key, &self.list)?;
        for (k, v) in &self.tail {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// Walk the compiled history: stored records are spliced in raw; fresh messages go through
/// `normalise`, and their output becomes a new [`WireRecord`].
pub fn assemble(
    req: &CompiledRequest,
    list: &mut Vec<WireItem>,
    mut normalise: impl FnMut(&Message, &mut Vec<Value>) -> Result<(), ProviderError>,
) -> Result<Vec<WireRecord>, ProviderError> {
    let mut records = vec![];
    for wm in &req.wire_history {
        match wm {
            WireMessage::Stored(rec) => {
                let items: Vec<Box<RawValue>> = serde_json::from_str(&rec.items).map_err(|e| {
                    ProviderError::invalid_request(format!("corrupt wire record: {e}"))
                })?;
                list.extend(items.into_iter().map(WireItem::Raw));
            }
            WireMessage::Fresh(m) => {
                let mut items = vec![];
                normalise(m, &mut items)?;
                let text = serde_json::to_string(&items)
                    .map_err(|e| ProviderError::invalid_request(e.to_string()))?;
                records.push(WireRecord {
                    message_id: m.id.clone(),
                    route_key: req.route_key.clone(),
                    items: text,
                });
                list.extend(items.into_iter().map(WireItem::Json));
            }
        }
    }
    Ok(records)
}

pub fn data_url(mime: &str, b64: &str) -> String {
    format!("data:{mime};base64,{b64}")
}

/// Parse accumulated tool-input JSON; empty input means `{}`.
pub fn parse_tool_input(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

pub fn u64_at(v: &Value, path: &[&str]) -> u64 {
    let mut cur = v;
    for p in path {
        match cur.get(p) {
            Some(n) => cur = n,
            None => return 0,
        }
    }
    cur.as_u64().unwrap_or(0)
}

pub fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
