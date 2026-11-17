//! `cargo xtask feed build`: turn the feed sources into the signed static tree that goes
//! onto Azure Storage + CDN (09 §4, t38 `feeds/`).
//!
//! ```text
//! cargo xtask feed build --out <dir> [--src feeds] [--updates-dir dist]
//!                        [--previous-dir <currently published tree>] [--root HEX]…
//!                        [--only <kind>]… [--ephemeral] [--now TS]
//! ```
//!
//! | source | published path | key (env) |
//! |---|---|---|
//! | `crates/genie-catalog/catalog/bundled.json` | `catalog/v1/catalog.json` | `GENIE_FEED_KEY_CATALOG` |
//! | `feeds/directory/connectors/v1.json` | `directory/connectors/v1.json` | `GENIE_FEED_KEY_CONNECTORS` |
//! | `feeds/directory/skills/v1.json` | `directory/skills/v1.json` | `GENIE_FEED_KEY_SKILLS` |
//! | `feeds/notices/index.json` | `notices/index.json` | `GENIE_FEED_KEY_NOTICES` |
//! | `<updates-dir>/update-<channel>-<target>.json` | `updates/<channel>/<target>.json` | `GENIE_FEED_KEY_UPDATES` |
//! | `feeds/keys/keylist.json` + `.sig` (signed offline by the root key) | `keys/keylist.json` | none: copied |
//!
//! Each feed gets `version = max(source, previous published + 1)`, fresh
//! `issued_at`/`expires_at`, a content check, a detached signature, and is then verified the
//! way clients verify it (signature, expiry, no rollback vs `--previous-dir`).
//! `--ephemeral` signs with throwaway keys (CI dry run); without it a missing key is an error.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use crate::feed::{self, Kind};
use crate::opts::Opts;
use crate::timefmt;

pub struct Source {
    pub kind: Kind,
    pub from: PathBuf,
    pub to: String,
}

pub fn sources(root: &Path, src: &Path, updates: Option<&Path>) -> Result<Vec<Source>, String> {
    let mut v = vec![
        Source {
            kind: Kind::Catalog,
            from: root.join("crates/genie-catalog/catalog/bundled.json"),
            to: "catalog/v1/catalog.json".into(),
        },
        Source {
            kind: Kind::Connectors,
            from: src.join("directory/connectors/v1.json"),
            to: "directory/connectors/v1.json".into(),
        },
        Source {
            kind: Kind::Skills,
            from: src.join("directory/skills/v1.json"),
            to: "directory/skills/v1.json".into(),
        },
        Source {
            kind: Kind::Notices,
            from: src.join("notices/index.json"),
            to: "notices/index.json".into(),
        },
    ];
    if let Some(dir) = updates {
        let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("update-") && n.ends_with(".json"))
            .collect();
        names.sort();
        for n in names {
            let stem = &n["update-".len()..n.len() - ".json".len()];
            let (channel, target) = stem
                .split_once('-')
                .ok_or_else(|| format!("{n}: expected update-<channel>-<target>.json"))?;
            v.push(Source {
                kind: Kind::Updates,
                from: dir.join(&n),
                to: format!("updates/{channel}/{target}.json"),
            });
            // tauri-plugin-updater asks per arch; a universal build serves both.
            if target == "darwin-universal" {
                for arch in ["darwin-aarch64", "darwin-x86_64"] {
                    v.push(Source {
                        kind: Kind::Updates,
                        from: dir.join(&n),
                        to: format!("updates/{channel}/{arch}.json"),
                    });
                }
            }
        }
    }
    Ok(v)
}

fn env_name(kind: Kind) -> String {
    format!("GENIE_FEED_KEY_{}", kind.as_str().to_ascii_uppercase())
}

pub fn run(root: &Path, o: &Opts) -> Result<String, Vec<String>> {
    let one = |e: String| vec![e];
    let src = o.get("--src").map(PathBuf::from).unwrap_or_else(|| root.join("feeds"));
    let out = PathBuf::from(o.require("--out").map_err(one)?);
    let updates = o.get("--updates-dir").map(PathBuf::from);
    let previous = o.get("--previous-dir").map(PathBuf::from);
    let now = match o.get("--now") {
        Some(t) => timefmt::parse(t).map_err(one)?,
        None => timefmt::now(),
    };
    let roots: Vec<_> = o
        .all("--root")
        .into_iter()
        .map(feed::parse_public)
        .collect::<Result<_, _>>()
        .map_err(one)?;
    let ephemeral = o.has("--ephemeral");
    let key_for = |kind: Kind| -> Result<SigningKey, String> {
        if ephemeral {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
            return Ok(SigningKey::from_bytes(&seed));
        }
        let var = env_name(kind);
        let v = std::env::var(&var)
            .map_err(|_| format!("{var} is not set (or pass --ephemeral for a dry run)"))?;
        feed::parse_secret(&v)
    };
    let mut srcs = sources(root, &src, updates.as_deref()).map_err(one)?;
    // `--only <kind>` (repeatable): e.g. the release job publishes update manifests only.
    let only: Vec<Kind> = o
        .all("--only")
        .into_iter()
        .map(Kind::parse)
        .collect::<Result<_, _>>()
        .map_err(one)?;
    if !only.is_empty() {
        srcs.retain(|s| only.contains(&s.kind));
    }
    let report = build(&srcs, &out, previous.as_deref(), now, &key_for).map_err(|e| vec![e])?;
    if !only.is_empty() && !only.contains(&Kind::KeyList) {
        return Ok(report);
    }
    let kl = copy_keylist(&src, &out, &roots, now).map_err(one)?;
    Ok(format!("{report}; {kl}"))
}

/// Builds and verifies every source into `out`.
pub fn build(
    srcs: &[Source],
    out: &Path,
    previous: Option<&Path>,
    now: i64,
    key_for: &dyn Fn(Kind) -> Result<SigningKey, String>,
) -> Result<String, String> {
    let mut keys: Vec<(Kind, SigningKey)> = vec![];
    let mut n = 0;
    for s in srcs {
        let key = match keys.iter().find(|(k, _)| *k == s.kind) {
            Some((_, k)) => k.clone(),
            None => {
                let k = key_for(s.kind)?;
                keys.push((s.kind, k.clone()));
                k
            }
        };
        let (_, mut doc) = feed::read_json(&s.from)?;
        let prev = match previous {
            Some(p) if p.join(&s.to).is_file() => {
                Some(feed::read_json(&p.join(&s.to)).and_then(|(_, d)| feed::header(&d))?)
            }
            _ => None,
        };
        let obj = doc
            .as_object_mut()
            .ok_or_else(|| format!("{}: not a JSON object", s.from.display()))?;
        let src_version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        let version = src_version.max(prev.map(|p| p.version + 1).unwrap_or(1));
        obj.insert("version".into(), json!(version));
        feed::stamp(obj, now, s.kind.default_ttl_days(), false);
        // A darwin-universal manifest copied to a per-arch path names that arch.
        if s.kind == Kind::Updates
            && let Some(t) = s.to.rsplit('/').next().and_then(|f| f.strip_suffix(".json"))
        {
            obj.insert("target".into(), json!(t));
        }
        let errs = feed::validate(s.kind, &doc);
        if !errs.is_empty() {
            return Err(format!("{}: {}", s.from.display(), errs.join("; ")));
        }
        let dest = out.join(&s.to);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        feed::write_json(&dest, &doc)?;
        let body = std::fs::read(&dest).map_err(|e| e.to_string())?;
        let sig = feed::sign_bytes(&key, &body);
        std::fs::write(sig_of(&dest), &sig).map_err(|e| e.to_string())?;
        // Verify as a client would.
        feed::verify_bytes(&[key.verifying_key()], &body, &sig)?;
        let h = feed::header(&doc)?;
        let errs = feed::check_header(&h, s.kind, now, prev.as_ref());
        if !errs.is_empty() {
            return Err(format!("{}: {}", s.to, errs.join("; ")));
        }
        n += 1;
    }
    let pubs: Vec<String> = keys
        .iter()
        .map(|(k, sk)| format!("{}={}", k.as_str(), feed::public_hex(&sk.verifying_key())))
        .collect();
    Ok(format!(
        "built and verified {n} feeds in {} (keys: {})",
        out.display(),
        pubs.join(", ")
    ))
}

fn sig_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

/// The key list is signed offline with the root key; publishing only copies it.
fn copy_keylist(
    src: &Path,
    out: &Path,
    roots: &[ed25519_dalek::VerifyingKey],
    now: i64,
) -> Result<String, String> {
    let kl = src.join("keys/keylist.json");
    let sig = sig_of(&kl);
    if !kl.is_file() || !sig.is_file() {
        return Ok("no signed key list in feeds/keys/ (root key ceremony pending), not published".into());
    }
    let body = std::fs::read(&kl).map_err(|e| e.to_string())?;
    let s = std::fs::read_to_string(&sig).map_err(|e| e.to_string())?;
    if !roots.is_empty() {
        feed::verify_bytes(roots, &body, &s).map_err(|e| format!("key list: {e}"))?;
        let doc: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let h = feed::header(&doc)?;
        let errs = feed::check_header(&h, Kind::KeyList, now, None);
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }
    }
    let dest = out.join("keys/keylist.json");
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dest, &body).map_err(|e| e.to_string())?;
    std::fs::write(sig_of(&dest), s).map_err(|e| e.to_string())?;
    Ok(if roots.is_empty() {
        "key list copied (not verified: no --root)".into()
    } else {
        "key list verified against the root key and copied".into()
    })
}

/// `ci feeds`: dry-run build with throwaway keys into a temp dir.
pub fn dry_run(root: &Path) -> Result<String, Vec<String>> {
    let out = std::env::temp_dir().join(format!("genie-feeds-ci-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let srcs = sources(root, &root.join("feeds"), None).map_err(|e| vec![e])?;
    let key_for = |_: Kind| -> Result<SigningKey, String> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
        Ok(SigningKey::from_bytes(&seed))
    };
    let r = build(&srcs, &out, None, timefmt::now(), &key_for).map_err(|e| vec![e]);
    let _ = std::fs::remove_dir_all(&out);
    r.map(|_| format!("feeds: {} sources build, sign and verify", srcs.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(PathBuf::from)
            .unwrap()
    }

    #[test]
    fn repo_feeds_build_and_clients_accept_them() {
        let tmp = std::env::temp_dir().join(format!("genie-feedbuild-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let key = SigningKey::from_bytes(&[5; 32]);
        let key_for = |_: Kind| Ok(key.clone());
        let r = root();
        let srcs = sources(&r, &r.join("feeds"), None).unwrap();
        let now = timefmt::now();
        build(&srcs, &tmp.join("v1"), None, now, &key_for).unwrap();

        // genie-catalog decodes and verifies the published catalog (header fields ignored).
        let body = std::fs::read(tmp.join("v1/catalog/v1/catalog.json")).unwrap();
        let sig = std::fs::read_to_string(tmp.join("v1/catalog/v1/catalog.json.sig")).unwrap();
        let vk = genie_catalog::signature::parse_key(&feed::public_hex(&key.verifying_key())).unwrap();
        genie_catalog::signature::verify_with(&[vk], &body, &sig).unwrap();
        let cat = genie_catalog::decode(
            &String::from_utf8_lossy(&body),
            genie_catalog::Source::Fetched,
        )
        .unwrap();
        assert!(!cat.is_empty());
        assert!(cat.diagnostics.is_empty(), "{:?}", cat.diagnostics);

        // genie-skills accepts the skills directory.
        let body = std::fs::read(tmp.join("v1/directory/skills/v1.json")).unwrap();
        let sig = std::fs::read_to_string(tmp.join("v1/directory/skills/v1.json.sig")).unwrap();
        let pk = genie_skills::directory::parse_key(&feed::public_hex(&key.verifying_key())).unwrap();
        genie_skills::directory::verify_feed(
            &[pk],
            &genie_skills::SignedFeed { body, signature_b64: sig },
        )
        .unwrap();

        // A second build against the first as "previous" bumps every version.
        build(&srcs, &tmp.join("v2"), Some(&tmp.join("v1")), now + 60, &key_for).unwrap();
        for s in &srcs {
            let a = feed::header(&feed::read_json(&tmp.join("v1").join(&s.to)).unwrap().1).unwrap();
            let b = feed::header(&feed::read_json(&tmp.join("v2").join(&s.to)).unwrap().1).unwrap();
            assert!(b.version > a.version, "{}", s.to);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
