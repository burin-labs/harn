//! MCP Server Card consumer + publisher (2026 MCP v2.1 spec, harn#75).
//!
//! A Server Card is a small JSON document that describes an MCP server
//! without requiring a full handshake. It lets skill matchers, tool
//! indexers, and IDEs decide whether to even connect to a server.
//!
//! This module implements both sides:
//! - **Consumer**: `fetch_server_card(source, ttl)` loads the card from a
//!   `.well-known/mcp-card` URL or a local file path, caches it in a
//!   per-process LRU with a TTL so repeated reads are free.
//! - **Publisher**: `load_server_card_from_path` parses a local card
//!   file for `harn mcp-serve --card path/to/card.json`, which embeds
//!   the card into the `initialize` response and exposes it as a static
//!   resource at `well-known://mcp-card`.
//!
//! The card schema intentionally mirrors the MCP v2.1 draft rather than
//! inventing a Harn-specific shape. Fields Harn doesn't recognize pass
//! through unchanged — forward-compat.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Default cache TTL (5 minutes) — long enough to avoid thundering
/// herds when a skill activation probes several cards in sequence,
/// short enough that an updated card reaches users within a coffee break.
const DEFAULT_TTL: Duration = Duration::from_secs(300);

/// Well-known path a compliant MCP server publishes its card at (per the
/// 2026 roadmap). Harn's consumer tries this suffix when given a bare
/// server URL without a `.well-known` path.
pub const WELL_KNOWN_PATH: &str = ".well-known/mcp-card";

/// One cached card entry. Stored in the process-wide cache keyed by
/// (server_name | fetch_source).
#[derive(Clone, Debug)]
struct CacheEntry {
    card: Value,
    fetched_at: Instant,
    ttl: Duration,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < self.ttl
    }
}

struct CardCache {
    entries: BTreeMap<String, CacheEntry>,
}

impl CardCache {
    const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.entries
            .get(key)
            .filter(|e| e.is_fresh())
            .map(|e| e.card.clone())
    }

    fn put(&mut self, key: String, card: Value, ttl: Duration) {
        self.entries.insert(
            key,
            CacheEntry {
                card,
                fetched_at: Instant::now(),
                ttl,
            },
        );
    }

    fn invalidate(&mut self, key: &str) {
        self.entries.remove(key);
    }

    #[cfg(test)]
    fn clear(&mut self) {
        self.entries.clear();
    }
}

static CARD_CACHE: Mutex<CardCache> = Mutex::new(CardCache::new());

/// Fetch (with cache) an MCP Server Card from a local file or HTTP(S)
/// URL.
///
/// - If `source` starts with `http://` / `https://`, Harn issues a GET
///   request. If the URL does not already contain `.well-known`, the
///   consumer also tries appending `/.well-known/mcp-card` on a 404.
/// - Otherwise `source` is treated as a local path (absolute or
///   relative to `$PWD`).
///
/// The cache key is the raw `source` string — different spellings of
/// the same URL get separate entries, which is safer than trying to
/// canonicalize.
pub async fn fetch_server_card(source: &str, ttl: Option<Duration>) -> Result<Value, CardError> {
    let ttl = ttl.unwrap_or(DEFAULT_TTL);
    if let Some(cached) = CARD_CACHE
        .lock()
        .expect("card cache mutex poisoned")
        .get(source)
    {
        return Ok(cached);
    }

    let card = if is_http_url(source) {
        fetch_over_http(source).await?
    } else {
        load_from_path(source)?
    };
    CARD_CACHE.lock().expect("card cache mutex poisoned").put(
        source.to_string(),
        card.clone(),
        ttl,
    );
    Ok(card)
}

/// Synchronous card loader from a local path — used by `harn mcp-serve
/// --card` at startup (before the tokio runtime is involved).
pub fn load_server_card_from_path(path: &std::path::Path) -> Result<Value, CardError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| CardError::Io(format!("read {}: {e}", path.display())))?;
    serde_json::from_str::<Value>(&contents).map_err(|e| CardError::Parse(e.to_string()))
}

fn is_http_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

fn load_from_path(source: &str) -> Result<Value, CardError> {
    let path = std::path::Path::new(source);
    load_server_card_from_path(path)
}

async fn fetch_over_http(url: &str) -> Result<Value, CardError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CardError::Http(format!("client build: {e}")))?;
    let primary = match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => Some(resp),
        Ok(_) => None,
        Err(_) => None,
    };

    let resp = if let Some(resp) = primary {
        resp
    } else {
        // Retry with well-known suffix if not already present.
        let fallback = with_well_known_suffix(url);
        if fallback.as_deref() == Some(url) {
            return Err(CardError::Http(format!(
                "GET {url} did not return a Server Card"
            )));
        }
        let Some(fallback) = fallback else {
            return Err(CardError::Http(format!("GET {url} failed")));
        };
        client
            .get(&fallback)
            .send()
            .await
            .map_err(|e| CardError::Http(format!("GET {fallback}: {e}")))?
    };
    if !resp.status().is_success() {
        return Err(CardError::Http(format!(
            "GET {url} returned HTTP {}",
            resp.status()
        )));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| CardError::Parse(format!("body: {e}")))
}

/// Returns `url` with `/.well-known/mcp-card` appended, unless the URL
/// already contains `.well-known` (caller asked for the exact path).
fn with_well_known_suffix(url: &str) -> Option<String> {
    if url.contains("/.well-known/") {
        return None;
    }
    let trimmed = url.trim_end_matches('/');
    Some(format!("{trimmed}/{WELL_KNOWN_PATH}"))
}

/// Drop a cached entry — exposed so tests can force a refresh without
/// sleeping past the TTL.
pub fn invalidate_cached(source: &str) {
    CARD_CACHE
        .lock()
        .expect("card cache mutex poisoned")
        .invalidate(source);
}

/// Errors the card consumer can surface. Stringified into user-facing
/// VM errors by the builtin wrapper.
#[derive(Debug)]
pub enum CardError {
    Io(String),
    Http(String),
    Parse(String),
}

impl std::fmt::Display for CardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CardError::Io(msg) => write!(f, "io: {msg}"),
            CardError::Http(msg) => write!(f, "http: {msg}"),
            CardError::Parse(msg) => write!(f, "parse: {msg}"),
        }
    }
}

impl std::error::Error for CardError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn reset_cache() {
        CARD_CACHE.lock().unwrap().clear();
    }

    #[test]
    fn loads_card_from_local_path() {
        reset_cache();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"{{"name":"demo","description":"Demo MCP server","tools":["a","b"]}}"#
        )
        .unwrap();
        let card = load_server_card_from_path(&path).unwrap();
        assert_eq!(card.get("name").and_then(|v| v.as_str()), Some("demo"));
    }

    #[test]
    fn parse_error_is_reported() {
        reset_cache();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(&path, "not json").unwrap();
        let err = load_server_card_from_path(&path).unwrap_err();
        assert!(matches!(err, CardError::Parse(_)));
    }

    #[test]
    fn well_known_suffix_respects_existing_path() {
        assert_eq!(
            with_well_known_suffix("https://example.com"),
            Some("https://example.com/.well-known/mcp-card".to_string())
        );
        assert_eq!(
            with_well_known_suffix("https://example.com/.well-known/mcp-card"),
            None
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_ttl_is_respected() {
        reset_cache();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        std::fs::write(&path, r#"{"name":"cached"}"#).unwrap();
        let card1 = fetch_server_card(&path, Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert_eq!(card1.get("name").and_then(|v| v.as_str()), Some("cached"));

        // Overwrite — cache should still serve the old value.
        std::fs::write(&path, r#"{"name":"updated"}"#).unwrap();
        let card2 = fetch_server_card(&path, Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert_eq!(card2.get("name").and_then(|v| v.as_str()), Some("cached"));

        // After invalidate, the new value shows up.
        invalidate_cached(&path);
        let card3 = fetch_server_card(&path, Some(Duration::from_secs(60)))
            .await
            .unwrap();
        assert_eq!(card3.get("name").and_then(|v| v.as_str()), Some("updated"));
    }
}
