//! `cargo xtask schema [--check | --release]` (02 §11).
//!
//! * default: write `packages/protocol-ts/schema/{protocol.schema.json,methods.json}` and
//!   the generated TypeScript in `packages/protocol-ts/src/`.
//! * `--check`: regenerate in memory and fail on drift; then compare with the released
//!   baseline in `schema/released/` and fail on a breaking change without a major bump.
//! * `--release`: copy the current schema to `schema/released/` (run when shipping).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::ts;

struct Files(Vec<(PathBuf, String)>);

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

fn generate(pkg: &Path) -> Result<Files, String> {
    let b = genie_protocol::schema::bundle();
    let defs = b.schema["$defs"]
        .as_object()
        .cloned()
        .ok_or("schema has no $defs")?;
    let methods = b.methods["methods"]
        .as_array()
        .cloned()
        .ok_or("methods.json has no methods")?;
    let version = genie_protocol::PROTOCOL_VERSION;
    let package_json = format!(
        "{{\n  \"name\": \"@genie/protocol\",\n  \"version\": \"{version}\",\n  \"private\": true,\n  \"description\": \"Generated from crates/genie-protocol by `cargo xtask schema`. Do not edit.\",\n  \"type\": \"module\",\n  \"main\": \"src/index.ts\",\n  \"types\": \"src/index.ts\"\n}}\n"
    );
    let index = format!(
        "{}\nexport * from \"./types\";\nexport * from \"./methods\";\n\n/** Semver of the protocol these types were generated from (02 §10). */\nexport const PROTOCOL_VERSION = \"{version}\";\n",
        ts::HEADER
    );
    Ok(Files(vec![
        (pkg.join("package.json"), package_json),
        (pkg.join("schema/protocol.schema.json"), pretty(&b.schema)),
        (pkg.join("schema/methods.json"), pretty(&b.methods)),
        (pkg.join("src/types.ts"), ts::types_file(&defs)),
        (pkg.join("src/methods.ts"), ts::methods_file(&methods)?),
        (pkg.join("src/index.ts"), index),
    ]))
}

pub fn run(root: &Path, mode: &str) -> Result<String, Vec<String>> {
    let pkg = root.join("packages/protocol-ts");
    let files = generate(&pkg).map_err(|e| vec![e])?;
    match mode {
        "write" => {
            for (p, c) in &files.0 {
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d)
                        .map_err(|e| vec![format!("{}: {e}", d.display())])?;
                }
                std::fs::write(p, c).map_err(|e| vec![format!("{}: {e}", p.display())])?;
            }
            let released = pkg.join("schema/released");
            if !released.join("protocol.schema.json").exists() {
                release(&pkg).map_err(|e| vec![e])?;
            }
            Ok(format!(
                "wrote {} files to {}",
                files.0.len(),
                pkg.display()
            ))
        }
        "release" => {
            release(&pkg).map_err(|e| vec![e])?;
            Ok("schema/released updated".into())
        }
        _ => {
            let mut errs = Vec::new();
            for (p, c) in &files.0 {
                match std::fs::read_to_string(p) {
                    Ok(cur) if cur == *c => {}
                    Ok(_) => errs.push(format!(
                        "drift: {} is out of date; run `cargo xtask schema`",
                        p.display()
                    )),
                    Err(_) => errs.push(format!(
                        "drift: {} is missing; run `cargo xtask schema`",
                        p.display()
                    )),
                }
            }
            match compat(&pkg) {
                Ok(()) => {}
                Err(mut e) => errs.append(&mut e),
            }
            if errs.is_empty() {
                Ok("protocol schema: no drift, compatible with released".into())
            } else {
                Err(errs)
            }
        }
    }
}

fn release(pkg: &Path) -> Result<(), String> {
    let b = genie_protocol::schema::bundle();
    let dir = pkg.join("schema/released");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("protocol.schema.json"), pretty(&b.schema))
        .map_err(|e| e.to_string())?;
    std::fs::write(dir.join("methods.json"), pretty(&b.methods)).map_err(|e| e.to_string())?;
    Ok(())
}

fn major(v: &Value) -> u64 {
    v.as_str()
        .and_then(|s| s.split('.').next())
        .and_then(|m| m.parse().ok())
        .unwrap_or(0)
}

/// Compare the current schema with the released one (02 §10).
fn compat(pkg: &Path) -> Result<(), Vec<String>> {
    let dir = pkg.join("schema/released");
    let read = |f: &str| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(dir.join(f)).ok()?).ok()
    };
    let (Some(old_schema), Some(old_methods)) =
        (read("protocol.schema.json"), read("methods.json"))
    else {
        return Err(vec![format!(
            "no released baseline in {}; run `cargo xtask schema --release`",
            dir.display()
        )]);
    };
    let b = genie_protocol::schema::bundle();
    if major(&b.schema["version"]) > major(&old_schema["version"]) {
        return Ok(()); // major bump: anything goes
    }
    let breaks = breaking_changes(&old_schema, &b.schema, &old_methods, &b.methods);
    if breaks.is_empty() {
        Ok(())
    } else {
        Err(breaks
            .into_iter()
            .map(|b| format!("breaking change without a major bump: {b}"))
            .collect())
    }
}

fn strip_docs(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "description" | "default" | "title"))
                .map(|(k, v)| (k.clone(), strip_docs(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_docs).collect()),
        other => other.clone(),
    }
}

fn str_set(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Breaking = something removed, renamed, retyped or newly required. Additions
/// (methods, optional fields, event types, enum values) are minor.
pub fn breaking_changes(old: &Value, new: &Value, old_m: &Value, new_m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let names = |m: &Value| -> Vec<String> {
        m["methods"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let new_names = names(new_m);
    for n in names(old_m) {
        if !new_names.contains(&n) {
            out.push(format!("method `{n}` removed"));
        }
    }
    let empty = Map::new();
    let od = old["$defs"].as_object().unwrap_or(&empty);
    let nd = new["$defs"].as_object().unwrap_or(&empty);
    for (name, o) in od {
        let Some(n) = nd.get(name) else {
            out.push(format!("type `{name}` removed"));
            continue;
        };
        check_schema(name, o, n, &mut out);
    }
    out
}

fn check_schema(path: &str, o: &Value, n: &Value, out: &mut Vec<String>) {
    // enum values removed
    let (oe, ne) = (str_set(&o["enum"]), str_set(&n["enum"]));
    for v in &oe {
        if !ne.contains(v) {
            out.push(format!("`{path}`: enum value `{v}` removed"));
        }
    }
    // properties removed / retyped, newly required
    if let (Some(op), Some(np)) = (o["properties"].as_object(), n["properties"].as_object()) {
        for (k, ov) in op {
            match np.get(k) {
                None => out.push(format!("`{path}.{k}` removed")),
                Some(nv) if strip_docs(ov) != strip_docs(nv) => {
                    // nested inline objects: recurse; otherwise a type change
                    if ov.get("properties").is_some() || ov.get("enum").is_some() {
                        check_schema(&format!("{path}.{k}"), ov, nv, out);
                    } else {
                        out.push(format!("`{path}.{k}` changed type"));
                    }
                }
                _ => {}
            }
        }
    }
    let (or, nr) = (str_set(&o["required"]), str_set(&n["required"]));
    for r in &nr {
        if !or.contains(r) {
            out.push(format!("`{path}.{r}` became required"));
        }
    }
    // tagged unions: variants removed (by tag)
    for k in ["oneOf", "anyOf"] {
        if let (Some(oa), Some(na)) = (o[k].as_array(), n[k].as_array()) {
            let tag = |v: &Value| -> Option<String> {
                v["properties"]["type"]["const"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| v["$ref"].as_str().map(String::from))
            };
            let ntags: Vec<String> = na.iter().filter_map(tag).collect();
            for t in oa.iter().filter_map(tag) {
                if !ntags.contains(&t) {
                    out.push(format!("`{path}`: variant `{t}` removed"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_breaks_and_allows_additions() {
        let m = json!({"methods": [{"name": "a"}]});
        let old = json!({"$defs": {"X": {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                                   "E": {"enum": ["p", "q"]}}});
        let added = json!({"$defs": {"X": {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "string"}}, "required": ["a"]},
                                     "E": {"enum": ["p", "q", "r"]}, "Y": {}}});
        let m2 = json!({"methods": [{"name": "a"}, {"name": "b"}]});
        assert!(breaking_changes(&old, &added, &m, &m2).is_empty());

        let broke = json!({"$defs": {"X": {"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "string"}}, "required": ["a", "b"]},
                                     "E": {"enum": ["p"]}}});
        let b = breaking_changes(&old, &broke, &m2, &m);
        assert_eq!(b.len(), 4, "{b:?}"); // method b removed, a retyped, b required, q removed
    }
}
