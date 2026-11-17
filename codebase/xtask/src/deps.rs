//! Crate dependency rules (01 §2.1 rules 1–3), checked with `cargo metadata`.
//!
//! Direction is apps → client/daemon → core → subsystems → protocol.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Leaf subsystems under `genie-core`. They may depend on `genie-protocol` only.
const SUBSYSTEMS: &[&str] = &[
    "genie-providers",
    "genie-catalog",
    "genie-store",
    "genie-config",
    "genie-permissions",
    "genie-secrets",
    "genie-tools",
    "genie-mcp",
    "genie-skills",
    "genie-process",
    "genie-sandbox",
    "genie-browser",
    "genie-audit",
    "genie-markdown",
];

/// Allowed subsystem → subsystem edges (plain leaf utilities, SA-38).
const LEAF_EXCEPTIONS: &[(&str, &str)] = &[
    ("genie-tools", "genie-process"),
    ("genie-mcp", "genie-process"),
    ("genie-browser", "genie-process"),
    // Browser tools implement the `Tool` trait (06 §4, §10); genie-tools is a leaf framework.
    ("genie-browser", "genie-tools"),
    ("genie-providers", "genie-secrets"),
    ("genie-mcp", "genie-secrets"),
];

fn allowed(from: &str, to: &str) -> bool {
    match from {
        "genie-protocol" => false,
        f if SUBSYSTEMS.contains(&f) => {
            to == "genie-protocol" || LEAF_EXCEPTIONS.contains(&(f, to))
        }
        "genie-core" => to == "genie-protocol" || SUBSYSTEMS.contains(&to),
        "genie-client" => to == "genie-protocol",
        // The daemon shares the transport layer that lives in genie-client.
        "genie-daemon" => {
            to == "genie-protocol"
                || to == "genie-core"
                || to == "genie-client"
                || SUBSYSTEMS.contains(&to)
        }
        "genie-testkit" => to != "genie-testkit",
        // Apps (genie-cli, the Tauri side) may use anything but the test kit.
        _ => to != "genie-testkit",
    }
}

pub fn check(root: &Path) -> Result<String, Vec<String>> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| vec![format!("cargo metadata: {e}")])?;
    if !out.status.success() {
        return Err(vec![format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )]);
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| vec![e.to_string()])?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();

    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in &packages {
        let name = p["name"].as_str().unwrap_or_default().to_string();
        let internal: Vec<String> = p["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            // Normal and build deps only; dev-deps may cross layers for tests.
            .filter(|d| d["kind"].is_null() || d["kind"] == "build")
            .filter_map(|d| d["name"].as_str())
            .filter(|n| n.starts_with("genie-"))
            .map(String::from)
            .collect();
        graph.insert(name, internal);
    }

    let mut errors = vec![];
    for (from, tos) in &graph {
        for to in tos {
            if !allowed(from, to) {
                errors.push(format!(
                    "`{from}` must not depend on `{to}` (spec/01-architecture.md §2.1)"
                ));
            }
        }
    }
    let crates = graph
        .keys()
        // Apps are not crates of the 01 §2 graph.
        .filter(|k| k.starts_with("genie-") && *k != "genie-cli" && *k != "genie-desktop")
        .count();
    if crates != 19 {
        errors.push(format!(
            "expected 19 genie-* crates (01 §2), found {crates}"
        ));
    }
    if errors.is_empty() {
        Ok(format!(
            "dependency rules hold for {} packages",
            graph.len()
        ))
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert!(!allowed("genie-protocol", "genie-core"));
        assert!(allowed("genie-tools", "genie-process"));
        assert!(!allowed("genie-tools", "genie-permissions"));
        assert!(allowed("genie-core", "genie-tools"));
        assert!(!allowed("genie-core", "genie-daemon"));
        assert!(!allowed("genie-client", "genie-core"));
        assert!(!allowed("genie-cli", "genie-testkit"));
    }
}
