//! Release artifacts (09 §1, §5.3): `cargo xtask dist`, `sbom`, `notices`, `sign`.
//!
//! `dist [--target T] [--channel C] [--out DIR] [--base-url URL] [--no-build] [--desktop]`
//!   1. builds `genie` (CLI + daemon, one binary) with `GENIE_CHANNEL`, `GENIE_COMMIT`,
//!      `GENIE_BUILD_DATE` set, so `genie --version` shows them (D40);
//!      `--target universal-apple-darwin` builds both macOS arches and `lipo`s them;
//!   2. packs `genie-<version>-<channel>-<feed target>.{tar.gz|zip}` with the notices;
//!   3. writes the CycloneDX SBOM and `THIRD-PARTY-NOTICES.md` (09 §1, t35 item 4);
//!   4. signs each artifact with the updates feed key when `GENIE_FEED_KEY_UPDATES` is set;
//!   5. writes `SHA256SUMS` and an unsigned update-manifest draft
//!      (`update-<channel>-<target>.json`, sign it with `cargo xtask feed sign`);
//!   6. writes the per-channel Tauri config overlay `tauri.<channel>.conf.json` and, with
//!      `--desktop`, runs `pnpm tauri build --config <overlay>` in `apps/desktop` if present.
//!
//! OS code signing (`sign <macos|windows|linux> <files…>`) shells out to the platform tools
//! with identities read from environment variables; with none set it skips and says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ci::cmd;
use crate::feed;
use crate::opts::Opts;
use crate::timefmt;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const UPDATES_KEY_ENV: &str = "GENIE_FEED_KEY_UPDATES";
/// Placeholder CDN origin until the Azure Storage + CDN endpoint exists (09 §4).
pub const DEFAULT_BASE_URL: &str = "https://feeds.genie.invalid";

/// Rust target triple → feed target name (`updates/<channel>/<os>-<arch>.json`).
pub fn feed_target(triple: &str) -> Result<&'static str, String> {
    Ok(match triple {
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => "windows-x86_64",
        "aarch64-pc-windows-msvc" => "windows-aarch64",
        "universal-apple-darwin" => "darwin-universal",
        "aarch64-apple-darwin" => "darwin-aarch64",
        "x86_64-apple-darwin" => "darwin-x86_64",
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "x86_64-unknown-linux-musl" => "linux-x86_64-musl",
        "aarch64-unknown-linux-musl" => "linux-aarch64-musl",
        other => return Err(format!("no release target for `{other}`")),
    })
}

fn host_triple() -> Result<String, String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("rustc -vV: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        .ok_or_else(|| "rustc -vV printed no host".into())
}

fn commit(root: &Path) -> String {
    if let Ok(s) = std::env::var("GITHUB_SHA") {
        return s.chars().take(12).collect();
    }
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// `SOURCE_DATE_EPOCH` (reproducible builds) or now.
fn build_time() -> i64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(timefmt::now)
}

pub fn run(root: &Path, sub: &str, args: Vec<String>) -> Result<String, Vec<String>> {
    let one = |e: String| vec![e];
    let o = Opts::parse(args).map_err(one)?;
    let out_dir = PathBuf::from(o.get("--out").unwrap_or("dist"));
    let out_dir = if out_dir.is_absolute() { out_dir } else { root.join(out_dir) };
    match sub {
        "sbom" => {
            std::fs::create_dir_all(&out_dir).map_err(|e| one(e.to_string()))?;
            let g = graph(root).map_err(one)?;
            let p = out_dir.join(format!("genie-{VERSION}.cdx.json"));
            write(&p, &sbom(&g, build_time())).map_err(one)?;
            Ok(format!("wrote {} ({} components)", p.display(), g.len()))
        }
        "notices" => {
            std::fs::create_dir_all(&out_dir).map_err(|e| one(e.to_string()))?;
            let g = graph(root).map_err(one)?;
            let p = out_dir.join("THIRD-PARTY-NOTICES.md");
            std::fs::write(&p, notices(root, &g)).map_err(|e| one(e.to_string()))?;
            Ok(format!("wrote {}", p.display()))
        }
        "sign" => sign_os(&o),
        _ => dist(root, &o, &out_dir),
    }
}

fn dist(root: &Path, o: &Opts, out_dir: &Path) -> Result<String, Vec<String>> {
    let one = |e: String| vec![e];
    let channel = o
        .get("--channel")
        .map(String::from)
        .or_else(|| std::env::var("GENIE_CHANNEL").ok())
        .unwrap_or_else(|| "dev".into());
    channel
        .parse::<genie_protocol::Channel>()
        .map_err(one)?;
    let triple = match o.get("--target") {
        Some(t) => t.to_string(),
        None => host_triple().map_err(one)?,
    };
    let target = feed_target(&triple).map_err(one)?;
    let windows = triple.contains("windows");
    let commit = commit(root);
    let built = build_time();
    let build_date = timefmt::format(built);
    std::fs::create_dir_all(out_dir).map_err(|e| one(e.to_string()))?;

    // 1. build
    let exe = if windows { "genie.exe" } else { "genie" };
    let binary = if o.has("--no-build") {
        locate_binary(root, &triple, exe)
    } else {
        build(root, &triple, &channel, &commit, &build_date)?;
        locate_binary(root, &triple, exe)
    }
    .map_err(one)?;

    // 2. stage + notices + sbom
    let base = format!("genie-{VERSION}-{channel}-{target}");
    let stage = out_dir.join(&base);
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| one(e.to_string()))?;
    std::fs::copy(&binary, stage.join(exe)).map_err(|e| one(format!("copy binary: {e}")))?;
    let g = graph(root).map_err(one)?;
    std::fs::write(stage.join("THIRD-PARTY-NOTICES.md"), notices(root, &g))
        .map_err(|e| one(e.to_string()))?;
    let sbom_path = out_dir.join(format!("genie-{VERSION}.cdx.json"));
    write(&sbom_path, &sbom(&g, built)).map_err(one)?;

    // archive
    let archive = if windows {
        let a = format!("{base}.zip");
        cmd(out_dir, "tar", &["-a", "-c", "-f", &a, &base])?;
        a
    } else {
        let a = format!("{base}.tar.gz");
        cmd(out_dir, "tar", &["-czf", &a, &base])?;
        a
    };
    let _ = std::fs::remove_dir_all(&stage);

    // 4. feed-key signatures (artifact signatures in the update manifest)
    let key = std::env::var(UPDATES_KEY_ENV).ok().filter(|k| !k.trim().is_empty());
    let mut artifacts = vec![];
    let apath = out_dir.join(&archive);
    let bytes = std::fs::read(&apath).map_err(|e| one(e.to_string()))?;
    let signature = match &key {
        Some(k) => {
            let sk = feed::parse_secret(k).map_err(one)?;
            let s = feed::sign_bytes(&sk, &bytes);
            std::fs::write(out_dir.join(format!("{archive}.sig")), &s)
                .map_err(|e| one(e.to_string()))?;
            s
        }
        None => String::new(),
    };
    let base_url = o.get("--base-url").unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
    artifacts.push(json!({
        "kind": "cli",
        "format": if windows { "zip" } else { "tar.gz" },
        "url": format!("{base_url}/releases/{VERSION}/{archive}"),
        "sha256": hex::encode(Sha256::digest(&bytes)),
        "size": bytes.len(),
        "signature": signature,
    }));

    // 5. checksums over everything in dist/ that is a release file
    let sums = checksums(out_dir).map_err(one)?;
    std::fs::write(out_dir.join("SHA256SUMS"), sums).map_err(|e| one(e.to_string()))?;
    let manifest = update_manifest(&channel, target, &commit, built, artifacts);
    let mpath = out_dir.join(format!("update-{channel}-{target}.json"));
    write(&mpath, &manifest).map_err(one)?;

    // 6. desktop overlay
    let overlay = tauri_overlay(root, &channel, base_url).map_err(one)?;
    let opath = out_dir.join(format!("tauri.{channel}.conf.json"));
    write(&opath, &overlay).map_err(one)?;
    let mut msg = format!(
        "{archive}, SBOM, SHA256SUMS, {} and {} in {}{}",
        mpath.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
        opath.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
        out_dir.display(),
        if key.is_none() {
            format!(" (unsigned: {UPDATES_KEY_ENV} not set)")
        } else {
            String::new()
        }
    );
    if o.has("--desktop") {
        msg.push_str(&format!("; {}", desktop(root, &channel, &opath, &commit, &build_date)?));
    }
    Ok(msg)
}

fn build(
    root: &Path,
    triple: &str,
    channel: &str,
    commit: &str,
    date: &str,
) -> Result<(), Vec<String>> {
    let triples: Vec<&str> = if triple == "universal-apple-darwin" {
        vec!["aarch64-apple-darwin", "x86_64-apple-darwin"]
    } else {
        vec![triple]
    };
    for t in &triples {
        let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "--release", "--locked", "-p", "genie-cli", "--bin", "genie", "--target", t])
            .env("GENIE_CHANNEL", channel)
            .env("GENIE_COMMIT", commit)
            .env("GENIE_BUILD_DATE", date)
            .current_dir(root)
            .status()
            .map_err(|e| vec![format!("cargo build: {e}")])?;
        if !status.success() {
            return Err(vec![format!("cargo build --target {t} failed")]);
        }
    }
    if triples.len() == 2 {
        let dir = root.join("target/universal-apple-darwin/release");
        std::fs::create_dir_all(&dir).map_err(|e| vec![e.to_string()])?;
        let a = root.join("target/aarch64-apple-darwin/release/genie");
        let x = root.join("target/x86_64-apple-darwin/release/genie");
        let out = dir.join("genie");
        cmd(
            root,
            "lipo",
            &[
                "-create",
                "-output",
                &out.to_string_lossy(),
                &a.to_string_lossy(),
                &x.to_string_lossy(),
            ],
        )?;
    }
    Ok(())
}

fn locate_binary(root: &Path, triple: &str, exe: &str) -> Result<PathBuf, String> {
    for p in [
        root.join("target").join(triple).join("release").join(exe),
        root.join("target/release").join(exe),
    ] {
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!("no built `{exe}` for {triple}; run without --no-build"))
}

pub fn update_manifest(
    channel: &str,
    target: &str,
    commit: &str,
    built: i64,
    artifacts: Vec<Value>,
) -> Value {
    let mut m = json!({
        "version": 1,
        "channel": channel,
        "target": target,
        "release": {
            "version": VERSION,
            "commit": commit,
            "pub_date": timefmt::format(built),
            "notes": "",
            "notes_url": format!("notices/{VERSION}.md"),
        },
        "min_version": null,
        "rollout": {
            // Stable starts at the first stage (09 §4.1); beta/dev ship to everyone.
            "percent": if channel == "stable" { feed::STABLE_STAGES[0] } else { 100 },
            "started_at": timefmt::format(built),
            "paused": false,
            "expedited": false,
        },
        "artifacts": artifacts,
    });
    if let Some(obj) = m.as_object_mut() {
        obj.remove("min_version");
        feed::stamp(obj, built, feed::Kind::Updates.default_ttl_days(), false);
    }
    m
}

fn checksums(dir: &Path) -> Result<String, String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "SHA256SUMS" && !n.ends_with(".sig"))
        .collect();
    names.sort();
    let mut out = String::new();
    for n in names {
        let b = std::fs::read(dir.join(&n)).map_err(|e| e.to_string())?;
        out.push_str(&format!("{}  {n}\n", hex::encode(Sha256::digest(&b))));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Channels and the Tauri overlay

pub fn channel_config(root: &Path, channel: &str) -> Result<Value, String> {
    let p = root.join("release/channels").join(format!("{channel}.json"));
    let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))
}

/// A config file passed to `tauri build --config`: name, bundle id and updater endpoint
/// per channel (09 §3) so channels install side by side. The desktop's own
/// `tauri.conf.json` stays the base; this only overrides channel-specific keys.
pub fn tauri_overlay(root: &Path, channel: &str, base_url: &str) -> Result<Value, String> {
    let c = channel_config(root, channel)?;
    let pubkey = std::env::var("GENIE_TAURI_UPDATER_PUBKEY").unwrap_or_default();
    Ok(json!({
        "productName": c["app_name"],
        "identifier": c["bundle_id"],
        "version": VERSION,
        "plugins": {
            "updater": {
                "endpoints": [format!("{base_url}/updates/{channel}/{{{{target}}}}-{{{{arch}}}}.json")],
                "pubkey": pubkey,
                "windows": { "installMode": "passive" }
            }
        },
        "bundle": {
            "createUpdaterArtifacts": true,
            "windows": { "wix": { "upgradeCode": c["windows"]["upgrade_code"] } },
            "linux": { "deb": { "depends": ["libwebkit2gtk-4.1-0", "libgtk-3-0"] } }
        }
    }))
}

fn desktop(
    root: &Path,
    channel: &str,
    overlay: &Path,
    commit: &str,
    date: &str,
) -> Result<String, Vec<String>> {
    let app = root.join("apps/desktop");
    if !app.join("package.json").is_file() {
        return Err(vec![
            "apps/desktop is not there yet; desktop bundling needs the Tauri app (spec 05)".into(),
        ]);
    }
    let status = Command::new("pnpm")
        .args(["tauri", "build", "--config", &overlay.to_string_lossy()])
        .env("GENIE_CHANNEL", channel)
        .env("GENIE_COMMIT", commit)
        .env("GENIE_BUILD_DATE", date)
        .current_dir(&app)
        .status()
        .map_err(|e| vec![format!("pnpm tauri build: {e}")])?;
    if status.success() {
        Ok("desktop bundles built".into())
    } else {
        Err(vec!["pnpm tauri build failed".into()])
    }
}

// ---------------------------------------------------------------------------------------
// OS code signing (09 §1). Identities come from CI secrets by name; missing → skipped.

fn sign_os(o: &Opts) -> Result<String, Vec<String>> {
    let os = o.pos(0, "macos|windows|linux").map_err(|e| vec![e])?;
    let files: Vec<&str> = o.positional.iter().skip(1).map(String::as_str).collect();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match os {
        "macos" => {
            let Some(identity) = env("APPLE_SIGNING_IDENTITY") else {
                return Ok("macOS signing skipped: APPLE_SIGNING_IDENTITY not set".into());
            };
            for f in &files {
                cmd(
                    Path::new("."),
                    "codesign",
                    &["--force", "--options", "runtime", "--timestamp", "--sign", &identity, f],
                )?;
            }
            // Notarise: one submission for the CLI archive / app bundle (09 §1).
            match (env("APPLE_API_KEY_PATH"), env("APPLE_API_KEY_ID"), env("APPLE_API_ISSUER")) {
                (Some(k), Some(id), Some(iss)) => {
                    for f in &files {
                        let zip = format!("{f}.notarize.zip");
                        cmd(Path::new("."), "ditto", &["-c", "-k", "--keepParent", f, &zip])?;
                        cmd(
                            Path::new("."),
                            "xcrun",
                            &["notarytool", "submit", &zip, "--key", &k, "--key-id", &id, "--issuer", &iss, "--wait"],
                        )?;
                        let _ = std::fs::remove_file(&zip);
                    }
                    Ok(format!("signed and notarised {} file(s)", files.len()))
                }
                _ => Ok(format!(
                    "signed {} file(s); notarisation skipped (APPLE_API_KEY_* not set)",
                    files.len()
                )),
            }
        }
        "windows" => {
            // Azure Trusted Signing through signtool + the Trusted Signing dlib; the
            // metadata.json naming account/profile is written by the workflow.
            let (Some(dlib), Some(meta)) = (env("TRUSTED_SIGNING_DLIB"), env("TRUSTED_SIGNING_METADATA"))
            else {
                return Ok("Windows signing skipped: TRUSTED_SIGNING_DLIB / _METADATA not set".into());
            };
            for f in &files {
                cmd(
                    Path::new("."),
                    "signtool",
                    &["sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256", "/dlib", &dlib, "/dmdf", &meta, f],
                )?;
            }
            Ok(format!("Authenticode-signed {} file(s)", files.len()))
        }
        "linux" => {
            let Some(keyid) = env("GPG_KEY_ID") else {
                return Ok("Linux signing skipped: GPG_KEY_ID not set".into());
            };
            for f in &files {
                cmd(
                    Path::new("."),
                    "gpg",
                    &["--batch", "--yes", "--local-user", &keyid, "--armor", "--detach-sign", f],
                )?;
            }
            Ok(format!("GPG-signed {} file(s)", files.len()))
        }
        other => Err(vec![format!("unknown OS `{other}` (macos|windows|linux)")]),
    }
}

// ---------------------------------------------------------------------------------------
// Dependency graph, SBOM, notices

#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub source: Option<String>,
    pub manifest: PathBuf,
}

/// Packages that ship in `genie` (normal + build deps reachable from genie-cli; dev deps
/// excluded; all platforms, so it slightly over-approximates any single target).
pub fn graph(root: &Path) -> Result<Vec<Pkg>, String> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let pkgs: BTreeMap<String, &Value> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?.to_string(), p)))
        .collect();
    let nodes: BTreeMap<String, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["id"].as_str()?.to_string(), n)))
        .collect();
    let start = pkgs
        .iter()
        .find(|(_, p)| p["name"] == "genie-cli")
        .map(|(id, _)| id.clone())
        .ok_or("genie-cli not in cargo metadata")?;
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for d in nodes.get(&id).and_then(|n| n["deps"].as_array()).into_iter().flatten() {
            let shipped = d["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].as_str() != Some("dev"));
            if shipped && let Some(p) = d["pkg"].as_str() {
                stack.push(p.to_string());
            }
        }
    }
    let mut v: Vec<Pkg> = seen
        .iter()
        .filter_map(|id| pkgs.get(id))
        .map(|p| Pkg {
            name: p["name"].as_str().unwrap_or_default().into(),
            version: p["version"].as_str().unwrap_or_default().into(),
            license: p["license"].as_str().map(String::from),
            repository: p["repository"].as_str().map(String::from),
            source: p["source"].as_str().map(String::from),
            manifest: PathBuf::from(p["manifest_path"].as_str().unwrap_or_default()),
        })
        .collect();
    v.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(v)
}

/// CycloneDX 1.5 JSON.
pub fn sbom(pkgs: &[Pkg], built: i64) -> Value {
    let components: Vec<Value> = pkgs
        .iter()
        .map(|p| {
            let ours = p.source.is_none();
            let mut c = json!({
                "type": if p.name == "genie-cli" { "application" } else { "library" },
                "bom-ref": format!("{}@{}", p.name, p.version),
                "name": p.name,
                "version": p.version,
                "scope": "required",
            });
            if !ours {
                c["purl"] = json!(format!("pkg:cargo/{}@{}", p.name, p.version));
            }
            if let Some(l) = &p.license {
                c["licenses"] = json!([{ "expression": l }]);
            }
            if let Some(r) = &p.repository {
                c["externalReferences"] = json!([{ "type": "vcs", "url": r }]);
            }
            c
        })
        .collect();
    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "timestamp": timefmt::format(built),
            "component": {
                "type": "application",
                "bom-ref": format!("genie-cli@{VERSION}"),
                "name": "genie",
                "version": VERSION,
                "supplier": { "name": "Synergech Technologies Pvt Ltd" }
            },
            "tools": { "components": [{ "type": "application", "name": "cargo xtask sbom" }] }
        },
        "components": components,
    })
}

/// Third-party notices: one section per crate (licence, repository) with the licence
/// texts shipped in the crate source, deduplicated; then `release/notices/extra.md`
/// (bundled tools, OpenCode MIT for adapted code, npm deps once the desktop exists).
pub fn notices(root: &Path, pkgs: &[Pkg]) -> String {
    let mut s = String::from(
        "# Third-party notices\n\nGenie includes the following open-source components.\n\n",
    );
    let mut texts: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for p in pkgs.iter().filter(|p| p.source.is_some()) {
        s.push_str(&format!(
            "- **{} {}**: {}{}\n",
            p.name,
            p.version,
            p.license.as_deref().unwrap_or("see crate"),
            p.repository.as_ref().map(|r| format!(" ({r})")).unwrap_or_default()
        ));
        let dir = p.manifest.parent().unwrap_or(Path::new("."));
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|f| {
                f.is_file()
                    && f.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy().to_ascii_uppercase();
                        n.starts_with("LICENSE") || n.starts_with("LICENCE") || n.starts_with("COPYING") || n.starts_with("NOTICE")
                    })
            })
            .collect();
        files.sort();
        for f in files {
            if let Ok(t) = std::fs::read_to_string(&f) {
                let key = hex::encode(Sha256::digest(t.trim().as_bytes()));
                texts
                    .entry(key)
                    .or_insert_with(|| (t, vec![]))
                    .1
                    .push(format!("{} {}", p.name, p.version));
            }
        }
    }
    s.push_str("\n## Licence texts\n");
    for (t, users) in texts.values() {
        s.push_str(&format!("\n### Used by: {}\n\n```text\n{}\n```\n", users.join(", "), t.trim()));
    }
    if let Ok(extra) = std::fs::read_to_string(root.join("release/notices/extra.md")) {
        s.push('\n');
        s.push_str(&extra);
    }
    s
}

fn write(path: &Path, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
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
    fn targets_map_to_feed_names() {
        for t in [
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "universal-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
        ] {
            let f = feed_target(t).unwrap();
            assert!(feed::TARGETS.contains(&f), "{f}");
        }
        assert!(feed_target("wasm32-unknown-unknown").is_err());
    }

    #[test]
    fn draft_manifest_is_valid_once_signed() {
        let art = json!({ "format": "tar.gz", "url": "https://x/genie.tar.gz",
            "sha256": "a".repeat(64), "size": 3, "signature": feed::sign_bytes(
                &ed25519_dalek::SigningKey::from_bytes(&[1; 32]), b"abc") });
        for ch in ["stable", "beta", "dev"] {
            let m = update_manifest(ch, "linux-x86_64", "abc", 1_794_873_600, vec![art.clone()]);
            assert!(feed::validate_update(&m).is_empty(), "{ch}: {:?}", feed::validate_update(&m));
            assert!(feed::header(&m).is_ok());
        }
    }

    /// release/channels/*.json agree with genie-protocol's channel naming (01 §1.2, 09 §3).
    #[test]
    fn channel_configs_match_protocol() {
        let mut ids = BTreeSet::new();
        for ch in [
            genie_protocol::Channel::Stable,
            genie_protocol::Channel::Beta,
            genie_protocol::Channel::Dev,
        ] {
            let c = channel_config(&root(), ch.as_str()).unwrap();
            assert_eq!(c["channel"], ch.as_str());
            assert_eq!(c["data_dir_suffix"], ch.dir_suffix());
            assert!(ids.insert(c["bundle_id"].as_str().unwrap().to_string()));
            let name = c["app_name"].as_str().unwrap();
            assert!(name.starts_with("Genie"));
            assert!(c["windows"]["upgrade_code"].as_str().is_some_and(|u| u.len() == 36));
            let o = tauri_overlay(&root(), ch.as_str(), DEFAULT_BASE_URL).unwrap();
            assert_eq!(o["identifier"], c["bundle_id"]);
        }
    }

    #[test]
    fn sbom_lists_shipped_crates_only() {
        let g = graph(&root()).unwrap();
        assert!(g.iter().any(|p| p.name == "genie-cli"));
        assert!(g.iter().any(|p| p.name == "tokio"));
        // genie-testkit is dev-only.
        assert!(!g.iter().any(|p| p.name == "genie-testkit"));
        let s = sbom(&g, 0);
        assert_eq!(s["bomFormat"], "CycloneDX");
        assert_eq!(s["components"].as_array().unwrap().len(), g.len());
    }
}
