//! Cached catalog + ETag refresh (03 §8.1 steps 2–3).
//!
//! `<data dir>/cache/catalog.json` + `.sig` + `.etag`. A fetched catalog is written only
//! after its signature verifies; a failed refresh keeps the last good copy. The ~6 h
//! schedule runs in `genie-core` on its `TaskScope` (no spawns here).

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;

use crate::signature::verify_with;
use crate::{Catalog, Source, decode};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub enum FeedResponse {
    NotModified,
    Body {
        catalog: Vec<u8>,
        signature: String,
        etag: Option<String>,
    },
}

/// The HTTP side, injected by `genie-core` (it goes through the shared Executor/proxy setup).
#[async_trait]
pub trait FeedFetcher: Send + Sync {
    async fn fetch(&self, url: &str, etag: Option<&str>) -> Result<FeedResponse, String>;
}

pub struct CatalogCache {
    dir: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    NotModified,
    Updated,
}

impl CatalogCache {
    /// `dir` is `<data dir>/cache`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    fn path(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("catalog.{ext}"))
    }
    pub fn etag(&self) -> Option<String> {
        std::fs::read_to_string(self.path("etag"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The cached copy if present, verified and decodable.
    pub fn load(&self, keys: &[VerifyingKey]) -> Result<Catalog, String> {
        let body =
            std::fs::read(self.path("json")).map_err(|e| format!("no cached catalog: {e}"))?;
        let sig = std::fs::read_to_string(self.path("sig"))
            .map_err(|e| format!("no cached signature: {e}"))?;
        verify_with(keys, &body, &sig).map_err(|e| e.to_string())?;
        decode(&String::from_utf8_lossy(&body), Source::Cached)
    }

    fn store(&self, body: &[u8], sig: &str, etag: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        write_atomic(&self.path("json"), body)?;
        write_atomic(&self.path("sig"), sig.as_bytes())?;
        match etag {
            Some(e) => write_atomic(&self.path("etag"), e.as_bytes()),
            None => match std::fs::remove_file(self.path("etag")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }

    /// One refresh: `If-None-Match`, verify, then replace the cache.
    pub async fn refresh(
        &self,
        fetcher: &dyn FeedFetcher,
        url: &str,
        keys: &[VerifyingKey],
    ) -> Result<RefreshOutcome, String> {
        match fetcher.fetch(url, self.etag().as_deref()).await? {
            FeedResponse::NotModified => Ok(RefreshOutcome::NotModified),
            FeedResponse::Body {
                catalog,
                signature,
                etag,
            } => {
                verify_with(keys, &catalog, &signature)
                    .map_err(|e| format!("catalog refresh rejected: {e}"))?;
                decode(&String::from_utf8_lossy(&catalog), Source::Fetched)?;
                self.store(&catalog, &signature, etag.as_deref())
                    .map_err(|e| e.to_string())?;
                Ok(RefreshOutcome::Updated)
            }
        }
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Startup load: the cached copy if it verifies and isn't older than the bundled one,
/// else the bundled snapshot. Diagnostics explain any fallback.
pub fn load_best(cache: &CatalogCache, keys: &[VerifyingKey]) -> Catalog {
    let bundled = Catalog::bundled();
    match cache.load(keys) {
        Ok(c) if c.version >= bundled.version => c,
        Ok(c) => {
            let mut b = bundled;
            b.diagnostics.push(crate::Diagnostic(format!(
                "cached catalog v{} older than bundled; using bundled",
                c.version
            )));
            b
        }
        Err(e) => {
            let mut b = bundled;
            b.diagnostics
                .push(crate::Diagnostic(format!("cached catalog not used: {e}")));
            b
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::tests::{key, sign};
    use std::sync::Mutex;

    struct Feed {
        responses: Mutex<Vec<Result<FeedResponse, String>>>,
        seen_etags: Mutex<Vec<Option<String>>>,
    }
    #[async_trait]
    impl FeedFetcher for Feed {
        async fn fetch(&self, _url: &str, etag: Option<&str>) -> Result<FeedResponse, String> {
            self.seen_etags.lock().unwrap().push(etag.map(String::from));
            self.responses.lock().unwrap().remove(0)
        }
    }

    fn doc(version: u32) -> Vec<u8> {
        serde_json::json!({"version": version, "models": [
            {"id": "m", "provider": "p", "protocol": "openai_chat", "wire_model_id": "m", "context_window": 1000, "max_output": 100}
        ]})
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn refresh_verifies_caches_and_uses_etag() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(dir.path().join("cache"));
        let keys = [key().verifying_key()];
        let good = doc(30_000_000);
        let feed = Feed {
            responses: Mutex::new(vec![
                Ok(FeedResponse::Body {
                    catalog: good.clone(),
                    signature: sign(&good),
                    etag: Some("\"v1\"".into()),
                }),
                Ok(FeedResponse::NotModified),
                Ok(FeedResponse::Body {
                    catalog: doc(30_000_001),
                    signature: sign(&good),
                    etag: None,
                }),
                Err("offline".into()),
            ]),
            seen_etags: Mutex::new(vec![]),
        };
        assert_eq!(
            cache.refresh(&feed, "u", &keys).await,
            Ok(RefreshOutcome::Updated)
        );
        assert_eq!(
            cache.refresh(&feed, "u", &keys).await,
            Ok(RefreshOutcome::NotModified)
        );
        assert!(
            cache
                .refresh(&feed, "u", &keys)
                .await
                .unwrap_err()
                .contains("rejected"),
            "bad signature"
        );
        assert!(cache.refresh(&feed, "u", &keys).await.is_err());
        assert_eq!(
            feed.seen_etags.lock().unwrap()[1].as_deref(),
            Some("\"v1\"")
        );
        // Last good copy is still there.
        let c = load_best(&cache, &keys);
        assert_eq!(c.source, Source::Cached);
        assert_eq!(c.version, 30_000_000);
    }

    #[test]
    fn falls_back_to_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(dir.path());
        let keys = [key().verifying_key()];
        let c = load_best(&cache, &keys);
        assert_eq!(c.source, Source::Bundled);
        assert!(!c.diagnostics.is_empty());
        // Tampered cache → bundled.
        let body = doc(30_000_000);
        cache.store(&body, &sign(&doc(1)), None).unwrap();
        assert_eq!(load_best(&cache, &keys).source, Source::Bundled);
        // Valid but older than bundled → bundled.
        let old = doc(0);
        cache.store(&old, &sign(&old), None).unwrap();
        assert_eq!(load_best(&cache, &keys).source, Source::Bundled);
    }
}
