use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone, Default)]
pub struct CatalogRefreshOptions {
    pub url: Option<String>,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogRefreshReport {
    pub status: String,
    pub refreshed: bool,
    pub source_url: String,
    pub cache_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub ttl_ms: u64,
    pub provider_count: usize,
    pub model_count: usize,
    pub alias_count: usize,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogCacheMetadata {
    source_url: String,
    fetched_at_ms: u64,
    ttl_ms: u64,
    etag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogDocument {
    #[serde(default, alias = "ttl_ms", alias = "ttlMS")]
    ttl_ms: Option<u64>,
    catalog: ProviderCatalogArtifact,
    #[serde(default)]
    signature: Option<CatalogDocumentSignature>,
}

#[derive(Debug, Deserialize)]
struct CatalogDocumentSignature {
    #[serde(default)]
    algorithm: String,
    key_id: String,
    signature: String,
}

pub(super) struct DecodedCatalogDocument {
    pub(super) artifact: ProviderCatalogArtifact,
    pub(super) ttl_ms: u64,
}

pub async fn refresh_runtime_catalog(options: CatalogRefreshOptions) -> CatalogRefreshReport {
    let source_url = options
        .url
        .clone()
        .or_else(|| env_nonempty(HARN_PROVIDER_CATALOG_URL_ENV))
        .unwrap_or_else(|| DEFAULT_PROVIDER_CATALOG_URL.to_string());
    let cache_dir = default_refresh_cache_dir();
    let cache_path = cache_dir.join(REMOTE_CACHE_BODY_FILE);
    if refresh_disabled() {
        return refresh_report(
            "disabled",
            false,
            source_url,
            cache_path,
            None,
            DEFAULT_REMOTE_TTL_MS,
            None,
        );
    }
    if crate::llm::current_agent_session_id().is_some() {
        return refresh_report(
            "skipped_agent_loop",
            false,
            source_url,
            cache_path,
            None,
            DEFAULT_REMOTE_TTL_MS,
            Some("catalog refresh is disabled inside a live agent loop".to_string()),
        );
    }

    if !options.force {
        if let Some((metadata, body)) = load_fresh_cached_catalog(&source_url, &cache_dir) {
            return install_remote_catalog_from_body(
                "cache_hit",
                false,
                &source_url,
                &cache_path,
                metadata.etag,
                &body,
                metadata.ttl_ms,
                allow_unsigned_for_url(&source_url),
            );
        }
    }

    let metadata = read_cache_metadata(&cache_dir).filter(|meta| meta.source_url == source_url);
    match fetch_remote_catalog(&source_url, metadata.as_ref()).await {
        Ok(FetchedCatalog::NotModified) => {
            if let Some((metadata, body)) = load_any_cached_catalog(&source_url, &cache_dir) {
                let _ = write_cache_metadata(
                    &cache_dir,
                    &CatalogCacheMetadata {
                        fetched_at_ms: now_ms(),
                        ..metadata.clone()
                    },
                );
                return install_remote_catalog_from_body(
                    "not_modified",
                    false,
                    &source_url,
                    &cache_path,
                    metadata.etag,
                    &body,
                    metadata.ttl_ms,
                    allow_unsigned_for_url(&source_url),
                );
            }
            refresh_report(
                "fallback",
                false,
                source_url,
                cache_path,
                None,
                DEFAULT_REMOTE_TTL_MS,
                Some("remote returned 304 but no cached catalog was available".to_string()),
            )
        }
        Ok(FetchedCatalog::Body { body, etag }) => {
            match decode_and_validate_document(&body, allow_unsigned_for_url(&source_url)) {
                Ok(decoded) => {
                    if let Err(error) = write_catalog_cache(
                        &cache_dir,
                        &body,
                        &CatalogCacheMetadata {
                            source_url: source_url.clone(),
                            fetched_at_ms: now_ms(),
                            ttl_ms: decoded.ttl_ms,
                            etag: etag.clone(),
                        },
                    ) {
                        eprintln!(
                            "[provider_catalog] warning: failed to write runtime catalog cache: {error}"
                        );
                    }
                    install_decoded_catalog(
                        "refreshed",
                        true,
                        source_url,
                        cache_path,
                        etag,
                        decoded,
                        None,
                    )
                }
                Err(error) => install_stale_or_fallback(
                    source_url,
                    cache_dir,
                    cache_path,
                    format!("remote catalog rejected: {error}"),
                ),
            }
        }
        Err(error) => install_stale_or_fallback(source_url, cache_dir, cache_path, error),
    }
}

fn install_stale_or_fallback(
    source_url: String,
    cache_dir: PathBuf,
    cache_path: PathBuf,
    warning: String,
) -> CatalogRefreshReport {
    eprintln!("[provider_catalog] warning: {warning}");
    if let Some((metadata, body)) = load_any_cached_catalog(&source_url, &cache_dir) {
        return install_remote_catalog_from_body(
            "stale_cache",
            false,
            &source_url,
            &cache_path,
            metadata.etag,
            &body,
            metadata.ttl_ms,
            allow_unsigned_for_url(&source_url),
        );
    }
    refresh_report(
        "fallback",
        false,
        source_url,
        cache_path,
        None,
        DEFAULT_REMOTE_TTL_MS,
        Some(warning),
    )
}

fn install_remote_catalog_from_body(
    status: &str,
    refreshed: bool,
    source_url: &str,
    cache_path: &std::path::Path,
    etag: Option<String>,
    body: &str,
    fallback_ttl_ms: u64,
    allow_unsigned: bool,
) -> CatalogRefreshReport {
    match decode_and_validate_document(body, allow_unsigned) {
        Ok(mut decoded) => {
            if decoded.ttl_ms == DEFAULT_REMOTE_TTL_MS {
                decoded.ttl_ms = fallback_ttl_ms;
            }
            install_decoded_catalog(
                status,
                refreshed,
                source_url.to_string(),
                cache_path.to_path_buf(),
                etag,
                decoded,
                None,
            )
        }
        Err(error) => refresh_report(
            "fallback",
            false,
            source_url.to_string(),
            cache_path.to_path_buf(),
            etag,
            fallback_ttl_ms,
            Some(format!("cached catalog rejected: {error}")),
        ),
    }
}

fn install_decoded_catalog(
    status: &str,
    refreshed: bool,
    source_url: String,
    cache_path: PathBuf,
    etag: Option<String>,
    decoded: DecodedCatalogDocument,
    warning: Option<String>,
) -> CatalogRefreshReport {
    let provider_count = decoded.artifact.providers.len();
    let model_count = decoded.artifact.models.len();
    let alias_count = decoded.artifact.aliases.len();
    crate::llm_config::set_runtime_catalog_overlay(Some(config_from_artifact(&decoded.artifact)));
    CatalogRefreshReport {
        status: status.to_string(),
        refreshed,
        source_url,
        cache_path: cache_path.display().to_string(),
        etag,
        ttl_ms: decoded.ttl_ms,
        provider_count,
        model_count,
        alias_count,
        warning,
    }
}

fn refresh_report(
    status: &str,
    refreshed: bool,
    source_url: String,
    cache_path: PathBuf,
    etag: Option<String>,
    ttl_ms: u64,
    warning: Option<String>,
) -> CatalogRefreshReport {
    let current = artifact();
    CatalogRefreshReport {
        status: status.to_string(),
        refreshed,
        source_url,
        cache_path: cache_path.display().to_string(),
        etag,
        ttl_ms,
        provider_count: current.providers.len(),
        model_count: current.models.len(),
        alias_count: current.aliases.len(),
        warning,
    }
}

enum FetchedCatalog {
    NotModified,
    Body { body: String, etag: Option<String> },
}

async fn fetch_remote_catalog(
    url: &str,
    metadata: Option<&CatalogCacheMetadata>,
) -> Result<FetchedCatalog, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;
    let mut request = client.get(url);
    if let Some(etag) = metadata.and_then(|meta| meta.etag.as_deref()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("failed to fetch runtime provider catalog: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchedCatalog::NotModified);
    }
    if !response.status().is_success() {
        return Err(format!(
            "runtime provider catalog fetch returned HTTP {}",
            response.status()
        ));
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to read runtime provider catalog body: {error}"))?;
    Ok(FetchedCatalog::Body { body, etag })
}

pub(super) fn decode_and_validate_document(
    body: &str,
    allow_unsigned: bool,
) -> Result<DecodedCatalogDocument, String> {
    if let Ok(artifact) = serde_json::from_str::<ProviderCatalogArtifact>(body) {
        if !allow_unsigned {
            return Err(format!(
                "unsigned provider catalog rejected; set {HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV}=1 only for trusted development sources"
            ));
        }
        validate_remote_artifact(artifact, DEFAULT_REMOTE_TTL_MS)
    } else {
        let document: CatalogDocument = serde_json::from_str(body)
            .map_err(|error| format!("catalog JSON does not match the runtime schema: {error}"))?;
        verify_document_signature(&document)?;
        validate_remote_artifact(
            document.catalog,
            document.ttl_ms.unwrap_or(DEFAULT_REMOTE_TTL_MS),
        )
    }
}

fn validate_remote_artifact(
    artifact: ProviderCatalogArtifact,
    ttl_ms: u64,
) -> Result<DecodedCatalogDocument, String> {
    let report = validate_artifact(&artifact);
    if !report.errors.is_empty() {
        return Err(report.errors.join("; "));
    }
    Ok(DecodedCatalogDocument {
        artifact,
        ttl_ms: ttl_ms.max(1),
    })
}

fn verify_document_signature(document: &CatalogDocument) -> Result<(), String> {
    let signature = document
        .signature
        .as_ref()
        .ok_or_else(|| "signed catalog envelope is missing signature metadata".to_string())?;
    if signature.algorithm != "ed25519" {
        return Err(format!(
            "unsupported catalog signature algorithm {}",
            signature.algorithm
        ));
    }
    let trusted_keys = trusted_catalog_keys()?;
    let public_key = trusted_keys.get(&signature.key_id).ok_or_else(|| {
        format!(
            "catalog signature key {} is not trusted; configure {HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV}",
            signature.key_id
        )
    })?;
    let canonical = serde_json::to_vec(&document.catalog)
        .map_err(|error| format!("failed to canonicalize signed catalog: {error}"))?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signature.signature)
        .map_err(|error| format!("catalog signature is not valid base64: {error}"))?;
    let signature = ed25519_dalek::Signature::from_slice(&signature_bytes)
        .map_err(|error| format!("catalog signature has invalid length: {error}"))?;
    public_key
        .verify(&canonical, &signature)
        .map_err(|error| format!("catalog signature did not verify: {error}"))
}

fn trusted_catalog_keys() -> Result<BTreeMap<String, ed25519_dalek::VerifyingKey>, String> {
    let mut keys = BTreeMap::new();
    let Some(raw) = env_nonempty(HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV) else {
        return Ok(keys);
    };
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (key_id, encoded) = entry
            .split_once('=')
            .or_else(|| entry.split_once(':'))
            .ok_or_else(|| {
                format!(
                    "{HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV} entries must use key_id=base64_public_key"
                )
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|error| format!("catalog public key {key_id} is not valid base64: {error}"))?;
        let public_key = ed25519_dalek::VerifyingKey::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| format!("catalog public key {key_id} must be 32 bytes"))?,
        )
        .map_err(|error| format!("catalog public key {key_id} is invalid: {error}"))?;
        keys.insert(key_id.trim().to_string(), public_key);
    }
    Ok(keys)
}

fn default_refresh_cache_dir() -> PathBuf {
    crate::runtime_paths::state_root(&crate::stdlib::process::runtime_root_base())
        .join("cache")
        .join(REMOTE_CACHE_DIR)
}

fn load_fresh_cached_catalog(
    source_url: &str,
    cache_dir: &std::path::Path,
) -> Option<(CatalogCacheMetadata, String)> {
    let (metadata, body) = load_any_cached_catalog(source_url, cache_dir)?;
    let age = now_ms().saturating_sub(metadata.fetched_at_ms);
    (age < metadata.ttl_ms).then_some((metadata, body))
}

fn load_any_cached_catalog(
    source_url: &str,
    cache_dir: &std::path::Path,
) -> Option<(CatalogCacheMetadata, String)> {
    let metadata = read_cache_metadata(cache_dir)?;
    if metadata.source_url != source_url {
        return None;
    }
    let body = std::fs::read_to_string(cache_dir.join(REMOTE_CACHE_BODY_FILE)).ok()?;
    Some((metadata, body))
}

fn read_cache_metadata(cache_dir: &std::path::Path) -> Option<CatalogCacheMetadata> {
    let body = std::fs::read_to_string(cache_dir.join(REMOTE_CACHE_META_FILE)).ok()?;
    serde_json::from_str(&body).ok()
}

fn write_catalog_cache(
    cache_dir: &std::path::Path,
    body: &str,
    metadata: &CatalogCacheMetadata,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(cache_dir.join(REMOTE_CACHE_BODY_FILE), body)?;
    write_cache_metadata(cache_dir, metadata)
}

fn write_cache_metadata(
    cache_dir: &std::path::Path,
    metadata: &CatalogCacheMetadata,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let body = serde_json::to_string_pretty(metadata).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(cache_dir.join(REMOTE_CACHE_META_FILE), body)
}

fn now_ms() -> u64 {
    harn_clock::now_wall_ms(&harn_clock::RealClock::new()).max(0) as u64
}

fn refresh_disabled() -> bool {
    matches!(
        env_nonempty(HARN_DISABLE_CATALOG_REFRESH_ENV)
            .as_deref()
            .map(|value| value.to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on")
    )
}

fn allow_unsigned_for_url(url: &str) -> bool {
    if matches!(
        env_nonempty(HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV)
            .as_deref()
            .map(|value| value.to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on")
    ) {
        return true;
    }
    url::Url::parse(url).ok().is_some_and(|parsed| {
        matches!(
            parsed.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("::1")
        )
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
