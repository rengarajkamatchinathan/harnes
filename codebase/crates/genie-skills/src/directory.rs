//! Signed skills directory (06 §9 "Sources", D37; 08 §2 supply chain): `directory.list` and
//! `directory.install` for `kind: "skills"`.
//!
//! The feed is a JSON document with a detached Ed25519 signature (base64 of the 64-byte
//! signature over the exact feed bytes), verified against pinned keys. Each entry pins a
//! version, the package's SHA-256 and its file list; install returns the "what will run"
//! manifest first and installs only on a second call with `confirm: true`, after checking
//! the download against the pinned checksum and file list.
//!
//! Fetching is a port ([`DirectoryTransport`]); the daemon implements it over HTTP.

use std::collections::BTreeSet;

use async_trait::async_trait;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use genie_protocol::api::mcp::{
    DirectoryEntry, DirectoryInstallParams, DirectoryInstallResult, DirectoryKind,
    DirectoryListResult,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::catalog::SkillCatalog;
use crate::error::SkillError;
use crate::package::{self, ImportOptions};
use crate::scan::SkillSource;

/// Pinned skills-directory keys (hex). Empty until the release signing key (D40) exists;
/// [`SkillDirectory::with_keys`] takes keys for tests and mirrors.
pub const PINNED_KEYS_HEX: &[&str] = &[];

/// The raw feed and its detached signature.
#[derive(Debug, Clone)]
pub struct SignedFeed {
    pub body: Vec<u8>,
    pub signature_b64: String,
}

#[async_trait]
pub trait DirectoryTransport: Send + Sync {
    async fn fetch_feed(&self) -> Result<SignedFeed, SkillError>;
    async fn fetch_package(&self, url: &str) -> Result<Vec<u8>, SkillError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub url: String,
    /// SHA-256 (hex) of the package.
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    /// Every file in the package (relative paths), shown on the details page.
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

impl FeedEntry {
    fn scripts(&self) -> Vec<&String> {
        self.files
            .iter()
            .filter(|f| f.starts_with("scripts/"))
            .collect()
    }

    /// The "what will run" manifest (details page and first `directory.install` call).
    pub fn manifest(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "version": self.version,
            "publisher": self.publisher,
            "sha256": self.sha256,
            "size": self.size,
            "files": self.files,
            "scripts": self.scripts(),
            "allowed_tools": self.allowed_tools,
            "scripts_note": "Scripts never run in chat. In Code mode and the CLI they run through the shell with your normal permissions.",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    pub version: u32,
    pub entries: Vec<FeedEntry>,
}

pub fn parse_key(hex_str: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(hex_str.trim()).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Verifies `feed` against `keys` and parses it.
pub fn verify_feed(keys: &[VerifyingKey], feed: &SignedFeed) -> Result<Feed, SkillError> {
    if keys.is_empty() {
        return Err(SkillError::Directory(
            "no directory signing keys are configured".into(),
        ));
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(feed.signature_b64.trim())
        .map_err(|_| SkillError::Directory("malformed feed signature".into()))?;
    let sig = Signature::from_slice(&raw)
        .map_err(|_| SkillError::Directory("malformed feed signature".into()))?;
    if !keys.iter().any(|k| k.verify(&feed.body, &sig).is_ok()) {
        return Err(SkillError::Directory(
            "feed signature does not verify".into(),
        ));
    }
    serde_json::from_slice(&feed.body).map_err(|e| SkillError::Directory(format!("bad feed: {e}")))
}

pub struct SkillDirectory<T> {
    transport: T,
    keys: Vec<VerifyingKey>,
}

impl<T: DirectoryTransport> SkillDirectory<T> {
    /// Uses the pinned production keys.
    pub fn new(transport: T) -> Self {
        let keys = PINNED_KEYS_HEX
            .iter()
            .filter_map(|k| parse_key(k))
            .collect();
        SkillDirectory { transport, keys }
    }

    pub fn with_keys(transport: T, keys: Vec<VerifyingKey>) -> Self {
        SkillDirectory { transport, keys }
    }

    async fn feed(&self) -> Result<Feed, SkillError> {
        verify_feed(&self.keys, &self.transport.fetch_feed().await?)
    }

    /// `directory.list {kind: "skills", query?}`: case-insensitive match on id, name and
    /// description. `extra` carries the details-page manifest.
    pub async fn list(&self, query: Option<&str>) -> Result<DirectoryListResult, SkillError> {
        let q = query.map(str::to_lowercase).unwrap_or_default();
        let entries = self
            .feed()
            .await?
            .entries
            .into_iter()
            .filter(|e| {
                q.is_empty()
                    || [&e.id, &e.name, &e.description]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&q))
            })
            .map(|e| DirectoryEntry {
                extra: e.manifest(),
                id: e.id,
                version: e.version,
                name: e.name,
                description: e.description,
            })
            .collect();
        Ok(DirectoryListResult { entries })
    }

    /// `directory.install {kind: "skills", id, version, confirm}`. Without `confirm` it
    /// returns the manifest and installs nothing. With it: download, size + SHA-256 + file
    /// list check, then a normal package import (replacing an older install) and a rescan.
    pub async fn install(
        &self,
        catalog: &mut SkillCatalog,
        params: &DirectoryInstallParams,
    ) -> Result<DirectoryInstallResult, SkillError> {
        if params.kind != DirectoryKind::Skills {
            return Err(SkillError::Directory(
                "not a skills directory request".into(),
            ));
        }
        if !catalog.policy().permits_id(&params.id) {
            return Err(SkillError::Blocked(params.id.clone()));
        }
        let feed = self.feed().await?;
        let entry = feed
            .entries
            .iter()
            .find(|e| e.id == params.id && e.version == params.version)
            .ok_or_else(|| {
                SkillError::Directory(format!(
                    "`{}` version {} is not in the directory",
                    params.id, params.version
                ))
            })?;
        let manifest = entry.manifest();
        if !params.confirm {
            return Ok(DirectoryInstallResult {
                installed: false,
                manifest,
            });
        }
        let root = catalog
            .user_root()
            .ok_or_else(|| SkillError::Policy("no user skills folder is configured".into()))?
            .to_path_buf();
        let bytes = self.transport.fetch_package(&entry.url).await?;
        let opts = ImportOptions {
            replace: true,
            source: SkillSource::Directory,
            expected_files: (!entry.files.is_empty())
                .then(|| entry.files.iter().cloned().collect::<BTreeSet<_>>()),
            version: Some(entry.version.clone()),
            ..ImportOptions::default()
        };
        if entry.size > 0 && bytes.len() as u64 != entry.size {
            return Err(SkillError::Directory(
                "package size does not match the directory".into(),
            ));
        }
        if !hex::encode(Sha256::digest(&bytes)).eq_ignore_ascii_case(&entry.sha256) {
            return Err(SkillError::Directory(
                "package checksum does not match the directory".into(),
            ));
        }
        let installed =
            package::import(&bytes, &root, &opts, &|id| id == entry.id).map_err(|e| match e {
                SkillError::Blocked(id) => SkillError::Directory(format!(
                    "package contains skill `{id}`, expected `{}`",
                    entry.id
                )),
                other => other,
            })?;
        catalog.rescan();
        Ok(DirectoryInstallResult {
            installed: true,
            manifest: json!({ "sha256": installed.sha256, "path": installed.path, "entry": manifest }),
        })
    }
}
