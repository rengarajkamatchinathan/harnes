//! Signed feeds (09 §4, D40, SA-31) and staged rollout (09 §4.1, SA-33).
//!
//! Signature format is the one `genie-catalog::signature::verify_with` and
//! `genie-skills::directory::verify_feed` check: a detached `.sig` file holding the base64
//! (standard alphabet) of the 64-byte Ed25519 signature over the exact feed bytes. Public
//! keys are 32 bytes, written as hex.
//!
//! Every feed carries the header `{version, issued_at, expires_at}`: `version` is a
//! monotonically increasing integer (clients reject a lower one than they hold), the
//! timestamps are UTC `YYYY-MM-DDTHH:MM:SSZ`.
//!
//! Commands (`cargo xtask feed <cmd>`):
//! - `build [--src feeds] --out <dir> [--previous-dir <published>]`: stamp + sign + verify
//!   every feed for publishing (see `feedbuild`).
//! - `keygen --out <file>`: new Ed25519 seed (hex, mode 0600); prints the public key.
//! - `stamp <feed.json> [--ttl-days N] [--bump]`: set `issued_at`/`expires_at` (and `version+1`).
//! - `sign <feed.json> (--key-env VAR | --key-file F)`: validate, then write `<feed.json>.sig`.
//! - `verify <feed.json> [--sig F] (--pubkey HEX)* [--keylist F --root HEX] [--previous F]
//!   [--now TS] [--artifacts-dir D] [--kind K]`.
//! - `rollout <manifest.json> (--next | --percent N | --pause | --resume) [--expedited]`.
//! - `gate --candidate PCT --baseline PCT --soak-hours H [--expedited]`: SA-33 decision.
//! - `bucket --install-id ID --version V [--percent N]`: reference rollout bucketing.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::opts::Opts;
use crate::timefmt;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Allowed clock skew for `issued_at` in the future.
const SKEW_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Updates,
    Catalog,
    Connectors,
    Skills,
    Notices,
    KeyList,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind, String> {
        Ok(match s {
            "updates" => Kind::Updates,
            "catalog" => Kind::Catalog,
            "connectors" => Kind::Connectors,
            "skills" => Kind::Skills,
            "notices" => Kind::Notices,
            "keylist" | "keys" => Kind::KeyList,
            o => {
                return Err(format!(
                    "unknown feed kind `{o}` (updates|catalog|connectors|skills|notices|keylist)"
                ));
            }
        })
    }

    /// From the published path layout of 09 §4.
    pub fn infer(path: &Path) -> Option<Kind> {
        let s = path.to_string_lossy().replace('\\', "/");
        if s.contains("updates/") {
            Some(Kind::Updates)
        } else if s.contains("catalog/") || s.ends_with("catalog.json") {
            Some(Kind::Catalog)
        } else if s.contains("connectors") {
            Some(Kind::Connectors)
        } else if s.contains("skills") {
            Some(Kind::Skills)
        } else if s.contains("notices/") {
            Some(Kind::Notices)
        } else if s.contains("keys/") || s.ends_with("keylist.json") {
            Some(Kind::KeyList)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Updates => "updates",
            Kind::Catalog => "catalog",
            Kind::Connectors => "connectors",
            Kind::Skills => "skills",
            Kind::Notices => "notices",
            Kind::KeyList => "keylist",
        }
    }

    /// Longest allowed `expires_at - issued_at`. Short for updates so a frozen mirror or
    /// a replayed old manifest stops working; a year for the root-signed key list.
    pub fn max_ttl_days(self) -> i64 {
        match self {
            Kind::Updates => 14,
            Kind::Catalog | Kind::Connectors | Kind::Skills | Kind::Notices => 30,
            Kind::KeyList => 400,
        }
    }

    pub fn default_ttl_days(self) -> i64 {
        match self {
            Kind::Updates => 7,
            Kind::Catalog | Kind::Connectors | Kind::Skills | Kind::Notices => 21,
            Kind::KeyList => 365,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Keys and signatures

pub fn parse_public(s: &str) -> Result<VerifyingKey, String> {
    let bytes: [u8; 32] = decode_32(s).ok_or_else(|| format!("`{s}` is not a 32-byte public key"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| format!("bad public key: {e}"))
}

/// A signing seed: 32 bytes as hex or base64 (secrets stores often hold base64).
pub fn parse_secret(s: &str) -> Result<SigningKey, String> {
    decode_32(s)
        .map(|b| SigningKey::from_bytes(&b))
        .ok_or_else(|| "signing key is not a 32-byte seed in hex or base64".to_string())
}

fn decode_32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    let v = if s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(s).ok()?
    } else {
        B64.decode(s).ok()?
    };
    v.try_into().ok()
}

pub fn public_hex(k: &VerifyingKey) -> String {
    hex::encode(k.to_bytes())
}

pub fn sign_bytes(key: &SigningKey, body: &[u8]) -> String {
    B64.encode(key.sign(body).to_bytes())
}

pub fn verify_bytes(keys: &[VerifyingKey], body: &[u8], sig_b64: &str) -> Result<(), String> {
    if keys.is_empty() {
        return Err("no verification keys given (--pubkey or --keylist/--root)".into());
    }
    let raw = B64
        .decode(sig_b64.trim())
        .map_err(|_| "signature is not base64".to_string())?;
    let sig = Signature::from_slice(&raw).map_err(|_| "signature is not 64 bytes".to_string())?;
    if keys.iter().any(|k| k.verify(body, &sig).is_ok()) {
        Ok(())
    } else {
        Err("signature does not verify against any given key".into())
    }
}

pub fn load_signing_key(o: &Opts) -> Result<SigningKey, String> {
    if let Some(var) = o.get("--key-env") {
        let v = std::env::var(var).map_err(|_| format!("environment variable {var} is not set"))?;
        parse_secret(&v)
    } else if let Some(f) = o.get("--key-file") {
        let v = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
        parse_secret(&v)
    } else {
        Err("give --key-env <VAR> or --key-file <path>".into())
    }
}

// ---------------------------------------------------------------------------------------
// Header

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u64,
    pub issued_at: i64,
    pub expires_at: i64,
}

pub fn header(doc: &Value) -> Result<Header, String> {
    let version = doc["version"]
        .as_u64()
        .filter(|v| *v > 0)
        .ok_or("header: `version` must be a positive integer")?;
    let ts = |k: &str| -> Result<i64, String> {
        doc[k]
            .as_str()
            .ok_or_else(|| format!("header: `{k}` is missing"))
            .and_then(|s| timefmt::parse(s).map_err(|e| format!("header: `{k}`: {e}")))
    };
    let h = Header {
        version,
        issued_at: ts("issued_at")?,
        expires_at: ts("expires_at")?,
    };
    if h.expires_at <= h.issued_at {
        return Err("header: `expires_at` must be after `issued_at`".into());
    }
    Ok(h)
}

/// Header rules at time `now` for a feed of `kind`; `previous` is the cached copy.
pub fn check_header(h: &Header, kind: Kind, now: i64, previous: Option<&Header>) -> Vec<String> {
    let mut errs = vec![];
    if h.expires_at - h.issued_at > kind.max_ttl_days() * 86_400 {
        errs.push(format!(
            "header: lifetime is over the {}-day maximum for {} feeds",
            kind.max_ttl_days(),
            kind.as_str()
        ));
    }
    if h.issued_at > now + SKEW_SECS {
        errs.push("header: `issued_at` is in the future".into());
    }
    if now >= h.expires_at {
        errs.push(format!(
            "header: feed expired at {}",
            timefmt::format(h.expires_at)
        ));
    }
    if let Some(p) = previous
        && h.version <= p.version
    {
        errs.push(format!(
            "rollback: version {} is not above the previous {}",
            h.version, p.version
        ));
    }
    errs
}

// ---------------------------------------------------------------------------------------
// Key list (SA-31): root-signed list naming one online key per feed type.

/// Keys from a verified key list valid for `kind` at `now`.
pub fn keys_from_keylist(
    body: &[u8],
    sig: &str,
    root: &[VerifyingKey],
    kind: Kind,
    now: i64,
) -> Result<Vec<VerifyingKey>, Vec<String>> {
    verify_bytes(root, body, sig).map_err(|e| vec![format!("key list: {e}")])?;
    let doc: Value = serde_json::from_slice(body).map_err(|e| vec![format!("key list: {e}")])?;
    let h = header(&doc).map_err(|e| vec![e])?;
    let errs = check_header(&h, Kind::KeyList, now, None);
    if !errs.is_empty() {
        return Err(errs);
    }
    let errs = validate_keylist(&doc);
    if !errs.is_empty() {
        return Err(errs);
    }
    let mut out = vec![];
    for k in doc["keys"].as_array().into_iter().flatten() {
        if k["feed"].as_str() != Some(kind.as_str()) {
            continue;
        }
        let nb = k["not_before"].as_str().and_then(|s| timefmt::parse(s).ok());
        let na = k["not_after"].as_str().and_then(|s| timefmt::parse(s).ok());
        if nb.is_some_and(|t| now < t) || na.is_some_and(|t| now >= t) {
            continue;
        }
        if let Some(pk) = k["public_key"].as_str().and_then(|s| parse_public(s).ok()) {
            out.push(pk);
        }
    }
    if out.is_empty() {
        return Err(vec![format!(
            "key list names no currently valid key for `{}` feeds",
            kind.as_str()
        )]);
    }
    Ok(out)
}

pub fn validate_keylist(doc: &Value) -> Vec<String> {
    let mut errs = vec![];
    let Some(keys) = doc["keys"].as_array() else {
        return vec!["key list: `keys` must be an array".into()];
    };
    for (i, k) in keys.iter().enumerate() {
        let at = format!("key list: keys[{i}]");
        if k["id"].as_str().is_none_or(str::is_empty) {
            errs.push(format!("{at}: `id` is required"));
        }
        match k["feed"].as_str().map(Kind::parse) {
            Some(Ok(Kind::KeyList)) => errs.push(format!("{at}: the root key is pinned, not listed")),
            Some(Ok(_)) => {}
            _ => errs.push(format!("{at}: `feed` must be a feed kind")),
        }
        if k["public_key"].as_str().map(parse_public).is_none_or(|r| r.is_err()) {
            errs.push(format!("{at}: `public_key` must be 32-byte hex"));
        }
        for t in ["not_before", "not_after"] {
            if let Some(s) = k[t].as_str()
                && timefmt::parse(s).is_err()
            {
                errs.push(format!("{at}: `{t}` is not a UTC timestamp"));
            }
        }
    }
    errs
}

// ---------------------------------------------------------------------------------------
// Update manifests: `updates/<channel>/<os>-<arch>.json`

pub const TARGETS: &[&str] = &[
    "windows-x86_64",
    "windows-aarch64",
    "darwin-universal",
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-x86_64",
    "linux-aarch64",
    "linux-x86_64-musl",
    "linux-aarch64-musl",
];

/// Stable rollout stages (09 §4.1). Beta and dev ship at 100%.
pub const STABLE_STAGES: &[u64] = &[10, 50, 100];

pub fn validate_update(doc: &Value) -> Vec<String> {
    let mut errs = vec![];
    let channel = doc["channel"].as_str().unwrap_or_default();
    if !["stable", "beta", "dev"].contains(&channel) {
        errs.push("update: `channel` must be stable, beta or dev".into());
    }
    if !doc["target"].as_str().is_some_and(|t| TARGETS.contains(&t)) {
        errs.push(format!("update: `target` must be one of {TARGETS:?}"));
    }
    let release = &doc["release"];
    let rel_version = release["version"].as_str().map(semver::Version::parse);
    if !matches!(rel_version, Some(Ok(_))) {
        errs.push("update: `release.version` must be semver".into());
    }
    if release["pub_date"].as_str().map(timefmt::parse).is_none_or(|r| r.is_err()) {
        errs.push("update: `release.pub_date` must be a UTC timestamp".into());
    }
    if let Some(mv) = doc.get("min_version").and_then(Value::as_str) {
        match (semver::Version::parse(mv), &rel_version) {
            (Ok(m), Some(Ok(r))) if m > *r => {
                errs.push("update: `min_version` is above `release.version`".into());
            }
            (Err(_), _) => errs.push("update: `min_version` must be semver".into()),
            _ => {}
        }
    }
    let r = &doc["rollout"];
    match r["percent"].as_u64() {
        Some(p) if p > 100 => errs.push("rollout: `percent` is over 100".into()),
        Some(p) if channel == "stable" && !STABLE_STAGES.contains(&p) && p != 0 => errs.push(
            format!("rollout: stable releases use the stages {STABLE_STAGES:?} (09 §4.1)"),
        ),
        Some(p) if channel != "stable" && p != 100 && p != 0 => {
            errs.push("rollout: beta/dev releases ship at 100% (or 0 when pulled)".into());
        }
        None => errs.push("rollout: `percent` is required".into()),
        _ => {}
    }
    if r["started_at"].as_str().map(timefmt::parse).is_none_or(|r| r.is_err()) {
        errs.push("rollout: `started_at` must be a UTC timestamp".into());
    }
    let arts = doc["artifacts"].as_array();
    if arts.is_none_or(Vec::is_empty) {
        errs.push("update: `artifacts` must list at least one artifact".into());
    }
    for (i, a) in arts.into_iter().flatten().enumerate() {
        let at = format!("artifacts[{i}]");
        if !a["url"].as_str().is_some_and(|u| u.starts_with("https://")) {
            errs.push(format!("{at}: `url` must be https"));
        }
        if !a["sha256"]
            .as_str()
            .is_some_and(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            errs.push(format!("{at}: `sha256` must be 64 hex characters"));
        }
        if a["size"].as_u64().is_none() {
            errs.push(format!("{at}: `size` is required"));
        }
        let sig_ok = a["signature"]
            .as_str()
            .and_then(|s| B64.decode(s).ok())
            .is_some_and(|b| b.len() == 64);
        if !sig_ok {
            errs.push(format!("{at}: `signature` must be a base64 Ed25519 signature"));
        }
        if a["format"].as_str().is_none_or(str::is_empty) {
            errs.push(format!("{at}: `format` is required"));
        }
    }
    errs
}

/// Checks each artifact file in `dir` (by URL basename) against its size, SHA-256 and
/// Ed25519 signature.
fn verify_artifacts(doc: &Value, dir: &Path, keys: &[VerifyingKey]) -> Vec<String> {
    let mut errs = vec![];
    for a in doc["artifacts"].as_array().into_iter().flatten() {
        let url = a["url"].as_str().unwrap_or_default();
        let name = url.rsplit('/').next().unwrap_or_default();
        let path = dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            errs.push(format!("{}: not found", path.display()));
            continue;
        };
        if a["size"].as_u64() != Some(bytes.len() as u64) {
            errs.push(format!("{name}: size differs from the manifest"));
        }
        if a["sha256"].as_str() != Some(hex::encode(Sha256::digest(&bytes)).as_str()) {
            errs.push(format!("{name}: SHA-256 differs from the manifest"));
        }
        if let Err(e) = verify_bytes(keys, &bytes, a["signature"].as_str().unwrap_or_default()) {
            errs.push(format!("{name}: {e}"));
        }
    }
    errs
}

// ---------------------------------------------------------------------------------------
// Staged rollout (09 §4.1, SA-33)

/// Which bucket (0–99) an installation falls in for a release. Stable across restarts and
/// independent between releases, so the same 10% doesn't always go first. Clients install
/// when `bucket < rollout.percent`. Reference for the desktop updater and `genie update`.
pub fn bucket(install_id: &str, version: &str) -> u64 {
    let d = Sha256::digest(format!("genie-rollout:{install_id}:{version}").as_bytes());
    let mut n = [0u8; 8];
    n.copy_from_slice(&d[..8]);
    u64::from_be_bytes(n) % 100
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gate {
    Advance,
    Hold(&'static str),
    Pause(&'static str),
}

pub const MIN_CRASH_FREE: f64 = 99.5;
pub const MAX_DROP: f64 = 0.2;
pub const SOAK_HOURS: f64 = 48.0;
pub const EXPEDITED_SOAK_HOURS: f64 = 24.0;

/// SA-33: soak ≥ 48 h (24 h expedited), crash-free sessions ≥ 99.5% and no more than
/// 0.2 points below the previous stable. A failed rate gate pauses the rollout.
pub fn gate(candidate: f64, baseline: f64, soak_hours: f64, expedited: bool) -> Gate {
    if candidate < MIN_CRASH_FREE {
        return Gate::Pause("crash-free session rate is below 99.5%");
    }
    if baseline - candidate > MAX_DROP + 1e-9 {
        return Gate::Pause("crash-free rate is more than 0.2 points below the previous stable");
    }
    let need = if expedited { EXPEDITED_SOAK_HOURS } else { SOAK_HOURS };
    if soak_hours < need {
        return Gate::Hold("soak time not reached");
    }
    Gate::Advance
}

pub fn next_stage(percent: u64) -> Option<u64> {
    STABLE_STAGES.iter().copied().find(|s| *s > percent)
}

// ---------------------------------------------------------------------------------------
// CLI

fn sig_path(feed: &Path, o: &Opts) -> PathBuf {
    o.get("--sig").map(PathBuf::from).unwrap_or_else(|| {
        let mut s = feed.as_os_str().to_owned();
        s.push(".sig");
        PathBuf::from(s)
    })
}

pub fn read_json(path: &Path) -> Result<(Vec<u8>, Value), String> {
    let body = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc = serde_json::from_slice(&body).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((body, doc))
}

pub fn write_json(path: &Path, doc: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
}

fn kind_of(o: &Opts, path: &Path) -> Result<Kind, String> {
    match o.get("--kind") {
        Some(k) => Kind::parse(k),
        None => Kind::infer(path)
            .ok_or_else(|| format!("{}: can't infer the feed kind; pass --kind", path.display())),
    }
}

fn now_from(o: &Opts) -> Result<i64, String> {
    o.get("--now").map(timefmt::parse).unwrap_or(Ok(timefmt::now()))
}

/// Content checks per kind (beyond the header).
pub fn validate(kind: Kind, doc: &Value) -> Vec<String> {
    match kind {
        Kind::Updates => validate_update(doc),
        Kind::KeyList => validate_keylist(doc),
        Kind::Catalog if !doc["models"].is_array() => vec!["catalog: `models` must be an array".into()],
        Kind::Connectors | Kind::Skills if !doc["entries"].is_array() => {
            vec![format!("{}: `entries` must be an array", kind.as_str())]
        }
        _ => vec![],
    }
}

pub fn run(root: &Path, args: Vec<String>) -> Result<String, Vec<String>> {
    let one = |e: String| vec![e];
    let mut it = args.into_iter();
    let cmd = it.next().unwrap_or_default();
    let o = Opts::parse(it).map_err(one)?;
    match cmd.as_str() {
        "build" => crate::feedbuild::run(root, &o),
        "keygen" => {
            let out = PathBuf::from(o.require("--out").map_err(one)?);
            if out.exists() && !o.has("--force") {
                return Err(one(format!("{} exists (use --force)", out.display())));
            }
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| one(format!("getrandom: {e}")))?;
            let sk = SigningKey::from_bytes(&seed);
            write_secret(&out, &hex::encode(seed)).map_err(one)?;
            Ok(format!(
                "wrote {} (keep it offline); public key {}",
                out.display(),
                public_hex(&sk.verifying_key())
            ))
        }
        "pubkey" => {
            let sk = load_signing_key(&o).map_err(one)?;
            Ok(public_hex(&sk.verifying_key()))
        }
        "stamp" => {
            let path = PathBuf::from(o.pos(0, "feed.json").map_err(one)?);
            let kind = kind_of(&o, &path).map_err(one)?;
            let (_, mut doc) = read_json(&path).map_err(one)?;
            let now = now_from(&o).map_err(one)?;
            let ttl: i64 = match o.get("--ttl-days") {
                Some(t) => t.parse().map_err(|_| one("--ttl-days must be a number".into()))?,
                None => kind.default_ttl_days(),
            };
            let obj = doc.as_object_mut().ok_or_else(|| one("feed must be a JSON object".into()))?;
            stamp(obj, now, ttl, o.has("--bump"));
            write_json(&path, &doc).map_err(one)?;
            Ok(format!(
                "stamped {} (version {}, expires {})",
                path.display(),
                doc["version"],
                doc["expires_at"].as_str().unwrap_or_default()
            ))
        }
        "sign" => {
            let path = PathBuf::from(o.pos(0, "feed.json").map_err(one)?);
            let kind = kind_of(&o, &path).map_err(one)?;
            let (body, doc) = read_json(&path).map_err(one)?;
            let now = now_from(&o).map_err(one)?;
            let h = header(&doc).map_err(one)?;
            let mut errs = check_header(&h, kind, now, None);
            errs.extend(validate(kind, &doc));
            if !errs.is_empty() {
                return Err(errs);
            }
            let sk = load_signing_key(&o).map_err(one)?;
            let sig = sign_bytes(&sk, &body);
            let out = sig_path(&path, &o);
            std::fs::write(&out, &sig).map_err(|e| one(format!("{}: {e}", out.display())))?;
            Ok(format!(
                "signed {} → {} (key {})",
                path.display(),
                out.display(),
                public_hex(&sk.verifying_key())
            ))
        }
        "sign-file" => {
            // Detached signature for a release artifact (same format), e.g. install scripts.
            let path = PathBuf::from(o.pos(0, "file").map_err(one)?);
            let body = std::fs::read(&path).map_err(|e| one(format!("{}: {e}", path.display())))?;
            let sk = load_signing_key(&o).map_err(one)?;
            let out = sig_path(&path, &o);
            std::fs::write(&out, sign_bytes(&sk, &body))
                .map_err(|e| one(format!("{}: {e}", out.display())))?;
            Ok(format!("signed {}", path.display()))
        }
        "verify" => verify_cmd(&o),
        "rollout" => {
            let path = PathBuf::from(o.pos(0, "manifest.json").map_err(one)?);
            let (_, mut doc) = read_json(&path).map_err(one)?;
            let now = now_from(&o).map_err(one)?;
            let msg = rollout(&mut doc, &o, now).map_err(one)?;
            let errs = validate_update(&doc);
            if !errs.is_empty() {
                return Err(errs);
            }
            write_json(&path, &doc).map_err(one)?;
            Ok(format!("{msg}; re-sign {} before publishing", path.display()))
        }
        "gate" => {
            let num = |k: &str| -> Result<f64, Vec<String>> {
                o.require(k)
                    .map_err(one)?
                    .parse()
                    .map_err(|_| one(format!("{k} must be a number")))
            };
            let g = gate(
                num("--candidate")?,
                num("--baseline")?,
                num("--soak-hours")?,
                o.has("--expedited"),
            );
            match g {
                Gate::Advance => Ok("advance: gate passed (SA-33)".into()),
                Gate::Hold(why) => Err(one(format!("hold: {why}"))),
                Gate::Pause(why) => Err(one(format!("pause: {why}; rollout must be paused"))),
            }
        }
        "bucket" => {
            let b = bucket(
                o.require("--install-id").map_err(one)?,
                o.require("--version").map_err(one)?,
            );
            match o.get("--percent").and_then(|p| p.parse::<u64>().ok()) {
                Some(p) => Ok(format!("bucket {b}: {}", if b < p { "in rollout" } else { "not yet" })),
                None => Ok(format!("bucket {b}")),
            }
        }
        other => Err(one(format!(
            "unknown `feed {other}`; expected build | keygen | pubkey | stamp | sign | sign-file | verify | rollout | gate | bucket"
        ))),
    }
}

pub fn stamp(obj: &mut Map<String, Value>, now: i64, ttl_days: i64, bump: bool) {
    if bump || obj.get("version").and_then(Value::as_u64).is_none() {
        let v = obj.get("version").and_then(Value::as_u64).unwrap_or(0) + 1;
        obj.insert("version".into(), json!(v));
    }
    obj.insert("issued_at".into(), json!(timefmt::format(now)));
    obj.insert(
        "expires_at".into(),
        json!(timefmt::format(now + ttl_days * 86_400)),
    );
}

fn rollout(doc: &mut Value, o: &Opts, now: i64) -> Result<String, String> {
    let r = doc
        .get_mut("rollout")
        .and_then(Value::as_object_mut)
        .ok_or("manifest has no `rollout` object")?;
    let msg = if o.has("--pause") {
        r.insert("paused".into(), json!(true));
        "rollout paused".to_string()
    } else if o.has("--resume") {
        r.insert("paused".into(), json!(false));
        "rollout resumed".to_string()
    } else {
        let cur = r.get("percent").and_then(Value::as_u64).unwrap_or(0);
        let p: u64 = if o.has("--next") {
            next_stage(cur).ok_or("rollout is already at 100%")?
        } else {
            o.require("--percent")?
                .parse()
                .map_err(|_| "--percent must be an integer")?
        };
        if p < cur {
            return Err(format!(
                "percent {p} is below the current {cur}; to roll back, publish the previous release with a higher feed version (release/runbooks/rollback.md)"
            ));
        }
        r.insert("percent".into(), json!(p));
        r.insert("started_at".into(), json!(timefmt::format(now)));
        r.insert("paused".into(), json!(false));
        format!("rollout at {p}%")
    };
    if o.has("--expedited") {
        r.insert("expedited".into(), json!(true));
    }
    let obj = doc.as_object_mut().ok_or("manifest must be a JSON object")?;
    stamp(obj, now, Kind::Updates.default_ttl_days(), true);
    Ok(msg)
}

fn verify_cmd(o: &Opts) -> Result<String, Vec<String>> {
    let one = |e: String| vec![e];
    let path = PathBuf::from(o.pos(0, "feed.json").map_err(one)?);
    let kind = kind_of(o, &path).map_err(one)?;
    let now = now_from(o).map_err(one)?;
    let (body, doc) = read_json(&path).map_err(one)?;
    let sp = sig_path(&path, o);
    let sig = std::fs::read_to_string(&sp).map_err(|e| one(format!("{}: {e}", sp.display())))?;

    let mut keys: Vec<VerifyingKey> = o
        .all("--pubkey")
        .into_iter()
        .map(parse_public)
        .collect::<Result<_, _>>()
        .map_err(one)?;
    if let Some(kl) = o.get("--keylist") {
        let root: Vec<VerifyingKey> = o
            .all("--root")
            .into_iter()
            .map(parse_public)
            .collect::<Result<_, _>>()
            .map_err(one)?;
        let klp = PathBuf::from(kl);
        let (kb, _) = read_json(&klp).map_err(one)?;
        let ks = std::fs::read_to_string(sig_path(&klp, &Opts::default()))
            .map_err(|e| one(format!("{kl}.sig: {e}")))?;
        keys.extend(keys_from_keylist(&kb, &ks, &root, kind, now)?);
    }

    let mut errs = vec![];
    if let Err(e) = verify_bytes(&keys, &body, &sig) {
        errs.push(e);
    }
    let previous = match o.get("--previous") {
        Some(p) => Some(
            read_json(Path::new(p))
                .and_then(|(_, d)| header(&d))
                .map_err(one)?,
        ),
        None => None,
    };
    match header(&doc) {
        Ok(h) => errs.extend(check_header(&h, kind, now, previous.as_ref())),
        Err(e) => errs.push(e),
    }
    errs.extend(validate(kind, &doc));
    if let Some(dir) = o.get("--artifacts-dir") {
        errs.extend(verify_artifacts(&doc, Path::new(dir), &keys));
    }
    if errs.is_empty() {
        Ok(format!("{} verifies ({} feed)", path.display(), kind.as_str()))
    } else {
        Err(errs)
    }
}

fn write_secret(path: &Path, content: &str) -> Result<(), String> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(content.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    const T0: i64 = 1_794_873_600; // 2026-11-17T00:00:00Z

    fn feed(version: u64, models: bool) -> Value {
        let mut v = json!({ "version": version });
        let obj = v.as_object_mut().unwrap();
        stamp(obj, T0, 21, false);
        if models {
            obj.insert("models".into(), json!([]));
        } else {
            obj.insert("entries".into(), json!([]));
        }
        v
    }

    #[test]
    fn same_format_as_genie_catalog_and_genie_skills() {
        let key = sk(9);
        let body = serde_json::to_vec(&feed(30_000_001, true)).unwrap();
        let sig = sign_bytes(&key, &body);
        let vk = key.verifying_key();
        // genie-catalog accepts our signature and key encoding.
        let ck = genie_catalog::signature::parse_key(&public_hex(&vk)).unwrap();
        assert_eq!(genie_catalog::signature::verify_with(&[ck], &body, &sig), Ok(()));
        // genie-skills too (its Feed ignores the extra header fields).
        let skills = serde_json::to_vec(&feed(3, false)).unwrap();
        let ssig = sign_bytes(&key, &skills);
        let pk = genie_skills::directory::parse_key(&public_hex(&vk)).unwrap();
        let parsed = genie_skills::directory::verify_feed(
            &[pk],
            &genie_skills::SignedFeed {
                body: skills.clone(),
                signature_b64: ssig.clone(),
            },
        )
        .unwrap();
        assert_eq!(parsed.version, 3);
        // And we verify theirs: one byte changed fails.
        assert_eq!(verify_bytes(&[vk], &skills, &ssig), Ok(()));
        let mut bad = skills;
        bad[0] ^= 1;
        assert!(verify_bytes(&[vk], &bad, &ssig).is_err());
    }

    #[test]
    fn header_rules() {
        let h = header(&feed(5, true)).unwrap();
        assert_eq!(h.expires_at - h.issued_at, 21 * 86_400);
        assert!(check_header(&h, Kind::Catalog, T0 + 60, None).is_empty());
        // expired
        assert!(!check_header(&h, Kind::Catalog, h.expires_at, None).is_empty());
        // issued in the future
        assert!(!check_header(&h, Kind::Catalog, T0 - 3600, None).is_empty());
        // too long for an update feed (14 days max)
        assert!(!check_header(&h, Kind::Updates, T0 + 60, None).is_empty());
        // rollback
        let prev = Header { version: 5, ..h };
        assert!(check_header(&h, Kind::Catalog, T0 + 60, Some(&prev))
            .iter()
            .any(|e| e.starts_with("rollback")));
        let older = Header { version: 4, ..h };
        assert!(check_header(&h, Kind::Catalog, T0 + 60, Some(&older)).is_empty());
        assert!(header(&json!({"version": 0, "issued_at": "x"})).is_err());
    }

    fn keylist(root: &SigningKey, online: &SigningKey, feed: &str) -> (Vec<u8>, String) {
        let mut v = json!({
            "version": 1,
            "keys": [{ "id": "k-2026", "feed": feed, "public_key": public_hex(&online.verifying_key()),
                       "not_before": "2026-01-01T00:00:00Z", "not_after": "2027-12-31T00:00:00Z" }]
        });
        stamp(v.as_object_mut().unwrap(), T0, 365, false);
        let body = serde_json::to_vec(&v).unwrap();
        let sig = sign_bytes(root, &body);
        (body, sig)
    }

    #[test]
    fn keylist_hierarchy() {
        let (root, online) = (sk(1), sk(2));
        let (body, sig) = keylist(&root, &online, "catalog");
        let keys =
            keys_from_keylist(&body, &sig, &[root.verifying_key()], Kind::Catalog, T0 + 1).unwrap();
        assert_eq!(keys, vec![online.verifying_key()]);
        // Wrong feed type: no key.
        assert!(keys_from_keylist(&body, &sig, &[root.verifying_key()], Kind::Updates, T0 + 1).is_err());
        // Not signed by the pinned root.
        assert!(keys_from_keylist(&body, &sig, &[sk(3).verifying_key()], Kind::Catalog, T0 + 1).is_err());
        // Past not_after.
        let late = timefmt::parse("2027-12-31T00:00:00Z").unwrap();
        assert!(keys_from_keylist(&body, &sig, &[root.verifying_key()], Kind::Catalog, late).is_err());
        // The root key can't be listed.
        let (b2, _) = keylist(&root, &online, "keylist");
        let d: Value = serde_json::from_slice(&b2).unwrap();
        assert!(!validate_keylist(&d).is_empty());
    }

    fn manifest(channel: &str, percent: u64) -> Value {
        let mut v = json!({
            "version": 1,
            "channel": channel,
            "target": "linux-x86_64",
            "release": { "version": "1.2.0", "pub_date": "2026-11-17T00:00:00Z", "notes": "n" },
            "min_version": "1.0.0",
            "rollout": { "percent": percent, "started_at": "2026-11-17T00:00:00Z", "paused": false },
            "artifacts": [{ "format": "tar.gz", "url": "https://dl.example/genie.tar.gz",
                            "sha256": "0".repeat(64), "size": 1, "signature": B64.encode([0u8; 64]) }]
        });
        stamp(v.as_object_mut().unwrap(), T0, 7, false);
        v
    }

    #[test]
    fn update_manifest_rules() {
        assert!(validate_update(&manifest("stable", 10)).is_empty());
        assert!(!validate_update(&manifest("stable", 30)).is_empty());
        assert!(validate_update(&manifest("beta", 100)).is_empty());
        assert!(!validate_update(&manifest("beta", 50)).is_empty());
        let mut m = manifest("stable", 10);
        m["min_version"] = json!("2.0.0");
        m["artifacts"][0]["url"] = json!("http://x");
        let errs = validate_update(&m);
        assert_eq!(errs.len(), 2, "{errs:?}");
    }

    #[test]
    fn rollout_advances_and_refuses_to_shrink() {
        let mut m = manifest("stable", 10);
        let o = Opts::parse(["--percent", "50"].map(String::from)).unwrap();
        rollout(&mut m, &o, T0 + 3600).unwrap();
        assert_eq!(m["rollout"]["percent"], 50);
        assert_eq!(m["version"], 2);
        let o = Opts::parse(["--percent", "10"].map(String::from)).unwrap();
        assert!(rollout(&mut m, &o, T0 + 7200).is_err());
        let o = Opts::parse(["--pause"].map(String::from)).unwrap();
        rollout(&mut m, &o, T0 + 7200).unwrap();
        assert_eq!(m["rollout"]["paused"], true);
        assert_eq!(next_stage(10), Some(50));
        assert_eq!(next_stage(100), None);
    }

    #[test]
    fn crash_gate_sa33() {
        assert_eq!(gate(99.8, 99.9, 49.0, false), Gate::Advance);
        assert!(matches!(gate(99.8, 99.9, 30.0, false), Gate::Hold(_)));
        assert_eq!(gate(99.8, 99.9, 30.0, true), Gate::Advance);
        assert!(matches!(gate(99.4, 99.4, 72.0, false), Gate::Pause(_)));
        assert!(matches!(gate(99.6, 99.9, 72.0, false), Gate::Pause(_)));
        assert_eq!(gate(99.7, 99.9, 72.0, false), Gate::Advance);
    }

    #[test]
    fn bucketing_is_stable_and_roughly_uniform() {
        assert_eq!(bucket("abc", "1.0.0"), bucket("abc", "1.0.0"));
        let in10 = (0..10_000)
            .filter(|i| bucket(&format!("id-{i}"), "1.2.0") < 10)
            .count();
        assert!((800..1200).contains(&in10), "{in10}");
    }

    #[test]
    fn secrets_parse_hex_and_base64() {
        let k = sk(4);
        let h = hex::encode(k.to_bytes());
        let b = B64.encode(k.to_bytes());
        assert_eq!(parse_secret(&h).unwrap().to_bytes(), k.to_bytes());
        assert_eq!(parse_secret(&b).unwrap().to_bytes(), k.to_bytes());
        assert!(parse_secret("nope").is_err());
    }
}
