//! Generated provider/model catalog artifact support.
//!
//! `llm_config` is the runtime source of truth. This module projects that
//! configuration plus the capability matrix into a stable JSON contract that
//! downstream products can vendor without re-parsing Harn's internal TOML or
//! Rust literals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::llm;
use crate::llm_config::{
    self, AliasDef, AliasToolCallingDef, ModelAvailability, ModelDef, ModelPricing, ProviderDef,
};

pub const PROVIDER_CATALOG_SCHEMA_VERSION: u32 = 2;
pub const PROVIDER_CATALOG_SCHEMA_ID: &str =
    "https://harnlang.com/schemas/provider-catalog.v2.json";
pub const PROVIDER_CATALOG_GENERATOR: &str = "harn providers export";
pub const HARN_DISABLE_CATALOG_REFRESH_ENV: &str = "HARN_DISABLE_CATALOG_REFRESH";
pub const HARN_PROVIDER_CATALOG_URL_ENV: &str = "HARN_PROVIDER_CATALOG_URL";
pub const HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV: &str = "HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED";
pub const HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV: &str = "HARN_PROVIDER_CATALOG_TRUSTED_KEYS";
pub const DEFAULT_PROVIDER_CATALOG_URL: &str =
    "https://burin-labs.github.io/harn-cloud/provider-catalog/provider-catalog.json";

const DEFAULT_REMOTE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const REMOTE_CACHE_DIR: &str = "provider-catalog";
const REMOTE_CACHE_BODY_FILE: &str = "catalog.json";
const REMOTE_CACHE_META_FILE: &str = "catalog.meta.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalogArtifact {
    pub schema_version: u32,
    pub schema: String,
    pub generated_by: String,
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
    pub aliases: Vec<CatalogAlias>,
    pub variants: Vec<CatalogVariant>,
    pub qc_defaults: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProvider {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub classification: ProviderClassification,
    pub endpoint: ProviderEndpoint,
    pub auth: ProviderAuth,
    pub protocols: Vec<String>,
    pub features: Vec<String>,
    pub caveats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClassification {
    Hosted,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url_env: Option<String>,
    pub chat_endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub style: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub env: Vec<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogAlias {
    pub name: String,
    pub model_id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<AliasToolCallingDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub aliases: Vec<String>,
    pub context_window: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_timeout: Option<f64>,
    pub modalities: ModelModalities,
    pub tool_support: ModelToolSupport,
    pub structured_output: String,
    pub format_preferences: ModelFormatPreferences,
    pub reasoning: ModelReasoning,
    pub prompt_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    pub deprecation: ModelDeprecation,
    pub availability: ModelAvailabilityStatus,
    pub quality_tags: Vec<String>,
    pub capability_tags: Vec<String>,
    pub family: String,
    pub lineage: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complementary_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid_as_reviewer_for: Vec<String>,
    /// Popular-consensus tier label: "small" | "mid" | "frontier" |
    /// "reasoning". Self-declared on the model row; the rule-based path
    /// is a fallback only.
    pub tier: String,
    /// True when weights are downloadable / self-hostable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weight: Option<bool>,
    /// Workload-shaped strength tags (coding, summarization, vision, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strengths: Vec<String>,
    /// Public benchmark numbers, snake_case identifier -> score.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub benchmarks: BTreeMap<String, f64>,
    /// Accelerated-serving ("fast mode") tier metadata, when offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<ModelFastMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailabilityStatus {
    Serverless,
    Dedicated,
    Unknown,
}

impl From<ModelAvailability> for ModelAvailabilityStatus {
    fn from(value: ModelAvailability) -> Self {
        match value {
            ModelAvailability::Serverless => Self::Serverless,
            ModelAvailability::Dedicated => Self::Dedicated,
            ModelAvailability::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolSupport {
    pub native: bool,
    pub text: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empirical_parity: Option<ModelToolEmpiricalParity>,
    pub tool_search: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tools: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolEmpiricalParity {
    pub verdict: String,
    pub preferred_format: String,
    pub confidence: String,
    pub sample_size: u32,
    pub last_evaluated: String,
    pub native_pass_rate: f64,
    pub text_pass_rate: f64,
    pub verifier_divergence_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFormatPreferences {
    pub prefers_xml_scaffolding: bool,
    pub prefers_markdown_scaffolding: bool,
    pub structured_output_mode: String,
    pub supports_assistant_prefill: bool,
    pub prefers_role_developer: bool,
    pub prefers_xml_tools: bool,
    pub thinking_block_style: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelReasoning {
    pub modes: Vec<String>,
    pub effort_supported: bool,
    pub none_supported: bool,
    pub interleaved_supported: bool,
    pub preserve_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDeprecation {
    pub status: DeprecationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Catalog id of the model that supersedes this one, when declared.
    /// Surfaces `ModelDef::superseded_by` as a machine-readable migration
    /// target so downstream consumers don't have to parse `note` prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeprecationStatus {
    Active,
    Deprecated,
}

/// Catalog projection of an accelerated-serving ("fast mode") tier.
/// Surfaces `ModelDef::fast_mode` so downstream consumers can show the
/// opt-in knob, premium pricing, and lifecycle without re-parsing the
/// source TOML. Absent on models with no faster serving path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFastMode {
    pub param: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beta_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otps_speedup: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogVariant {
    pub id: String,
    pub label: String,
    pub description: String,
    pub model_id: String,
    pub provider: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCatalogValidation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ProviderCatalogValidation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

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

struct DecodedCatalogDocument {
    artifact: ProviderCatalogArtifact,
    ttl_ms: u64,
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

fn decode_and_validate_document(
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

fn config_from_artifact(artifact: &ProviderCatalogArtifact) -> llm_config::ProvidersConfig {
    llm_config::ProvidersConfig {
        providers: artifact
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider_def_from_catalog(provider)))
            .collect(),
        aliases: artifact
            .aliases
            .iter()
            .map(|alias| {
                (
                    alias.name.clone(),
                    llm_config::AliasDef {
                        id: alias.model_id.clone(),
                        provider: alias.provider.clone(),
                        tool_format: alias.tool_format.clone(),
                    },
                )
            })
            .collect(),
        alias_tool_calling: artifact
            .aliases
            .iter()
            .filter_map(|alias| {
                alias
                    .tool_calling
                    .clone()
                    .map(|tool_calling| (alias.name.clone(), tool_calling))
            })
            .collect(),
        models: artifact
            .models
            .iter()
            .map(|model| (model.id.clone(), model_def_from_catalog(model)))
            .collect(),
        qc_defaults: artifact.qc_defaults.clone(),
        ..llm_config::ProvidersConfig::default()
    }
}

fn provider_def_from_catalog(provider: &CatalogProvider) -> llm_config::ProviderDef {
    llm_config::ProviderDef {
        display_name: Some(provider.display_name.clone()),
        icon: provider.icon.clone(),
        base_url: provider.endpoint.base_url.clone(),
        base_url_env: provider.endpoint.base_url_env.clone(),
        auth_style: provider.auth.style.clone(),
        auth_style_explicit: true,
        auth_header: provider.auth.header.clone(),
        auth_env: match provider.auth.env.as_slice() {
            [] => llm_config::AuthEnv::None,
            [one] => llm_config::AuthEnv::Single(one.clone()),
            many => llm_config::AuthEnv::Multiple(many.to_vec()),
        },
        chat_endpoint: provider.endpoint.chat_endpoint.clone(),
        completion_endpoint: provider.endpoint.completion_endpoint.clone(),
        features: provider.features.clone(),
        rpm: provider.rpm,
        latency_p50_ms: provider.latency_p50_ms,
        ..llm_config::ProviderDef::default()
    }
}

fn model_def_from_catalog(model: &CatalogModel) -> llm_config::ModelDef {
    llm_config::ModelDef {
        name: model.name.clone(),
        provider: model.provider.clone(),
        context_window: model.context_window,
        runtime_context_window: model.runtime_context_window,
        stream_timeout: model.stream_timeout,
        capabilities: model.capability_tags.clone(),
        pricing: model.pricing.clone(),
        deprecated: model.deprecation.status == DeprecationStatus::Deprecated,
        deprecation_note: model.deprecation.note.clone(),
        superseded_by: model.deprecation.superseded_by.clone(),
        fast_mode: model
            .fast_mode
            .as_ref()
            .map(|fast| llm_config::FastModeDef {
                param: fast.param.clone(),
                value: fast.value.clone(),
                beta_header: fast.beta_header.clone(),
                otps_speedup: fast.otps_speedup,
                status: fast.status.clone(),
                pricing: fast.pricing.clone(),
                note: fast.note.clone(),
            }),
        quality_tags: model.quality_tags.clone(),
        availability: match model.availability {
            ModelAvailabilityStatus::Serverless => llm_config::ModelAvailability::Serverless,
            ModelAvailabilityStatus::Dedicated => llm_config::ModelAvailability::Dedicated,
            ModelAvailabilityStatus::Unknown => llm_config::ModelAvailability::Unknown,
        },
        tier: Some(model.tier.clone()),
        open_weight: model.open_weight,
        strengths: model.strengths.clone(),
        benchmarks: model.benchmarks.clone(),
        family: Some(model.family.clone()),
        lineage: Some(model.lineage.clone()),
        complementary_with: model.complementary_with.clone(),
        avoid_as_reviewer_for: model.avoid_as_reviewer_for.clone(),
    }
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

pub fn artifact() -> ProviderCatalogArtifact {
    let alias_entries = llm_config::alias_entries();
    let aliases_by_model = aliases_by_model(&alias_entries);
    let providers = llm_config::provider_names()
        .into_iter()
        .filter_map(|id| {
            llm_config::provider_config(&id).map(|provider| catalog_provider(id, provider))
        })
        .collect();
    let models = llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| catalog_model(id, model, &aliases_by_model))
        .collect::<Vec<_>>();
    let aliases = alias_entries
        .iter()
        .map(|(name, alias)| catalog_alias(name, alias))
        .collect::<Vec<_>>();
    let variants = catalog_variants(&models, &aliases);

    ProviderCatalogArtifact {
        schema_version: PROVIDER_CATALOG_SCHEMA_VERSION,
        schema: PROVIDER_CATALOG_SCHEMA_ID.to_string(),
        generated_by: PROVIDER_CATALOG_GENERATOR.to_string(),
        providers,
        models,
        aliases,
        variants,
        qc_defaults: llm_config::qc_defaults(),
    }
}

pub fn artifact_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&artifact()).map(|mut text| {
        text.push('\n');
        text
    })
}

pub fn schema_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&schema_value()).map(|mut text| {
        text.push('\n');
        text
    })
}

pub fn typescript_binding() -> Result<String, serde_json::Error> {
    let json = artifact_json()?;
    Ok(format!(
        "{}{}{}{}{}",
        generated_header("//", "typescript"),
        TYPESCRIPT_TYPES,
        "\nexport const harnProviderCatalog: HarnProviderCatalog = ",
        json.trim_end(),
        ";\n",
    ) + TYPESCRIPT_COMPAT_EXPORTS)
}

pub fn swift_binding() -> Result<String, serde_json::Error> {
    let json = artifact_json()?;
    Ok(format!(
        "{}{}\npublic let harnProviderCatalogJSON = #\"\"\"\n{}\"\"\"#\n",
        generated_header("//", "swift"),
        SWIFT_TYPES,
        json
    ))
}

pub fn validate_artifact(artifact: &ProviderCatalogArtifact) -> ProviderCatalogValidation {
    let mut result = ProviderCatalogValidation::default();
    if artifact.schema_version != PROVIDER_CATALOG_SCHEMA_VERSION {
        result.errors.push(format!(
            "schema_version must be {}, got {}",
            PROVIDER_CATALOG_SCHEMA_VERSION, artifact.schema_version
        ));
    }
    if artifact.providers.is_empty() {
        result.errors.push("catalog has no providers".to_string());
    }
    if artifact.models.is_empty() {
        result.errors.push("catalog has no models".to_string());
    }

    let provider_ids: BTreeSet<_> = artifact.providers.iter().map(|p| p.id.as_str()).collect();
    for provider in &artifact.providers {
        if provider.id.trim().is_empty() {
            result
                .errors
                .push("provider id cannot be empty".to_string());
        }
        if provider.display_name.trim().is_empty() {
            result.errors.push(format!(
                "provider {} display_name cannot be empty",
                provider.id
            ));
        }
        if provider.endpoint.chat_endpoint.trim().is_empty() {
            result.errors.push(format!(
                "provider {} chat_endpoint cannot be empty",
                provider.id
            ));
        }
        if provider.auth.required
            && provider.auth.env.is_empty()
            && provider.auth.style != "aws_sigv4"
        {
            result.errors.push(format!(
                "provider {} requires auth but declares no auth env keys",
                provider.id
            ));
        }
    }

    let mut alias_names = BTreeSet::new();
    for alias in &artifact.aliases {
        if alias.name.trim().is_empty() {
            result.errors.push("alias name cannot be empty".to_string());
        }
        if !alias_names.insert(alias.name.as_str()) {
            result
                .errors
                .push(format!("duplicate alias name {}", alias.name));
        }
        if !provider_ids.contains(alias.provider.as_str()) {
            result.errors.push(format!(
                "alias {} references unknown provider {}",
                alias.name, alias.provider
            ));
        }
    }

    let mut model_ids = BTreeSet::new();
    let mut model_pairs = BTreeSet::new();
    for model in &artifact.models {
        if !model_ids.insert(model.id.as_str()) {
            result
                .errors
                .push(format!("duplicate model id {}", model.id));
        }
        model_pairs.insert((model.provider.as_str(), model.id.as_str()));
        if model.name.trim().is_empty() {
            result
                .errors
                .push(format!("model {} name cannot be empty", model.id));
        }
        if !provider_ids.contains(model.provider.as_str()) {
            result.errors.push(format!(
                "model {} references unknown provider {}",
                model.id, model.provider
            ));
        }
        validate_token_field(model, "family", &model.family, &mut result);
        validate_token_field(model, "lineage", &model.lineage, &mut result);
        for family in &model.complementary_with {
            validate_token_field(model, "complementary_with", family, &mut result);
        }
        for selector in &model.avoid_as_reviewer_for {
            validate_reviewer_selector(model, selector, &mut result);
        }
        if model.context_window == 0 {
            result.errors.push(format!(
                "model {} context_window must be positive",
                model.id
            ));
        }
        if let Some(pricing) = &model.pricing {
            validate_pricing(model, pricing, &mut result);
        }
        if model.deprecation.status == DeprecationStatus::Deprecated
            && model
                .deprecation
                .note
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            result.errors.push(format!(
                "deprecated model {} must include deprecation.note",
                model.id
            ));
        }
        if let Some(fast) = &model.fast_mode {
            if let Some(pricing) = &fast.pricing {
                validate_pricing(model, pricing, &mut result);
            }
            if let Some(status) = fast.status.as_deref() {
                if !matches!(status, "ga" | "research_preview" | "deprecated") {
                    result.warnings.push(format!(
                        "model {} fast_mode.status {:?} is not one of ga|research_preview|deprecated",
                        model.id, status
                    ));
                }
            }
        }
    }

    // Structured supersession pointers must reference a real catalog row so
    // `superseded_by` can be trusted as a migration target by downstream
    // tooling. A dangling pointer is a soft warning (the row is still
    // usable) rather than a hard error, mirroring how `note` is advisory.
    for model in &artifact.models {
        if let Some(target) = model.deprecation.superseded_by.as_deref() {
            if !model_ids.contains(target) {
                result.warnings.push(format!(
                    "model {} declares superseded_by {} with no matching catalog row",
                    model.id, target
                ));
            }
        }
    }

    let dedicated_pairs: BTreeSet<(&str, &str)> = artifact
        .models
        .iter()
        .filter(|model| model.availability == ModelAvailabilityStatus::Dedicated)
        .map(|model| (model.provider.as_str(), model.id.as_str()))
        .collect();
    for alias in &artifact.aliases {
        if !model_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str())) {
            result.errors.push(format!(
                "alias {} targets {}/{} without a catalog row",
                alias.name, alias.provider, alias.model_id
            ));
        }
        if is_tier_alias(&alias.name)
            && dedicated_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str()))
        {
            result.warnings.push(format!(
                "tier alias {} targets dedicated-only model {}/{}; serverless callers will fail until the dedicated endpoint is provisioned",
                alias.name, alias.provider, alias.model_id
            ));
        }
    }

    for variant in &artifact.variants {
        if variant.id.trim().is_empty() {
            result.errors.push("variant id cannot be empty".to_string());
        }
        if !provider_ids.contains(variant.provider.as_str()) {
            result.errors.push(format!(
                "variant {} references unknown provider {}",
                variant.id, variant.provider
            ));
        }
        if !model_pairs.contains(&(variant.provider.as_str(), variant.model_id.as_str())) {
            result.errors.push(format!(
                "variant {} targets {}/{} without a catalog row",
                variant.id, variant.provider, variant.model_id
            ));
        }
    }

    result
}

pub fn validate_current() -> ProviderCatalogValidation {
    validate_artifact(&artifact())
}

pub fn schema_value() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": PROVIDER_CATALOG_SCHEMA_ID,
        "title": "Harn provider catalog",
        "type": "object",
        "required": ["schema_version", "schema", "generated_by", "providers", "models", "aliases", "variants", "qc_defaults"],
        "properties": {
            "schema_version": {"const": PROVIDER_CATALOG_SCHEMA_VERSION},
            "schema": {"const": PROVIDER_CATALOG_SCHEMA_ID},
            "generated_by": {"type": "string"},
            "providers": {"type": "array", "items": {"$ref": "#/$defs/provider"}},
            "models": {"type": "array", "items": {"$ref": "#/$defs/model"}},
            "aliases": {"type": "array", "items": {"$ref": "#/$defs/alias"}},
            "variants": {"type": "array", "items": {"$ref": "#/$defs/variant"}},
            "qc_defaults": {"type": "object", "additionalProperties": {"type": "string"}}
        },
        "additionalProperties": false,
        "$defs": {
            "provider": {
                "type": "object",
                "required": ["id", "display_name", "classification", "endpoint", "auth", "protocols", "features", "caveats"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "display_name": {"type": "string", "minLength": 1},
                    "icon": {"type": "string"},
                    "classification": {"enum": ["hosted", "local"]},
                    "endpoint": {"$ref": "#/$defs/endpoint"},
                    "auth": {"$ref": "#/$defs/auth"},
                    "protocols": {"type": "array", "items": {"type": "string"}},
                    "features": {"type": "array", "items": {"type": "string"}},
                    "caveats": {"type": "array", "items": {"type": "string"}},
                    "rpm": {"type": "integer", "minimum": 1},
                    "latency_p50_ms": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            },
            "endpoint": {
                "type": "object",
                "required": ["base_url", "chat_endpoint"],
                "properties": {
                    "base_url": {"type": "string"},
                    "base_url_env": {"type": "string"},
                    "chat_endpoint": {"type": "string", "minLength": 1},
                    "completion_endpoint": {"type": "string"}
                },
                "additionalProperties": false
            },
            "auth": {
                "type": "object",
                "required": ["style", "env", "required"],
                "properties": {
                    "style": {"type": "string"},
                    "header": {"type": "string"},
                    "env": {"type": "array", "items": {"type": "string"}},
                    "required": {"type": "boolean"}
                },
                "additionalProperties": false
            },
            "alias": {
                "type": "object",
                "required": ["name", "model_id", "provider"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "model_id": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "tool_format": {"type": "string"},
                    "tool_calling": {
                        "type": "object",
                        "properties": {
                            "native": {"type": "string"},
                            "text": {"type": "string"},
                            "streaming_native": {"type": "string"},
                            "fallback_mode": {"type": "string"},
                            "failure_reason": {"type": "string"},
                            "last_probe_at": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            },
            "model": {
                "type": "object",
                "required": [
                    "id",
                    "name",
                    "provider",
                    "aliases",
                    "context_window",
                    "modalities",
                    "tool_support",
                    "structured_output",
                    "format_preferences",
                    "reasoning",
                    "prompt_cache",
                    "deprecation",
                    "availability",
                    "quality_tags",
                    "capability_tags",
                    "family",
                    "lineage",
                    "tier"
                ],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "name": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "aliases": {"type": "array", "items": {"type": "string"}},
                    "context_window": {"type": "integer", "minimum": 1},
                    "runtime_context_window": {"type": "integer", "minimum": 1},
                    "stream_timeout": {"type": "number", "exclusiveMinimum": 0},
                    "modalities": {"$ref": "#/$defs/modalities"},
                    "tool_support": {"$ref": "#/$defs/tool_support"},
                    "structured_output": {"type": "string"},
                    "format_preferences": {"$ref": "#/$defs/format_preferences"},
                    "reasoning": {"$ref": "#/$defs/reasoning"},
                    "prompt_cache": {"type": "boolean"},
                    "pricing": {"$ref": "#/$defs/pricing"},
                    "deprecation": {"$ref": "#/$defs/deprecation"},
                    "availability": {"enum": ["serverless", "dedicated", "unknown"]},
                    "quality_tags": {"type": "array", "items": {"type": "string"}},
                    "capability_tags": {"type": "array", "items": {"type": "string"}},
                    "family": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"},
                    "lineage": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"},
                    "complementary_with": {"type": "array", "items": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$"}},
                    "avoid_as_reviewer_for": {"type": "array", "items": {"type": "string", "minLength": 1}},
                    "tier": {"enum": ["small", "mid", "frontier", "reasoning"]},
                    "open_weight": {"type": "boolean"},
                    "strengths": {"type": "array", "items": {"type": "string"}},
                    "benchmarks": {"type": "object", "additionalProperties": {"type": "number"}},
                    "fast_mode": {"$ref": "#/$defs/fast_mode"}
                },
                "additionalProperties": false
            },
            "modalities": {
                "type": "object",
                "required": ["input", "output"],
                "properties": {
                    "input": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                    "output": {"type": "array", "items": {"type": "string"}, "minItems": 1}
                },
                "additionalProperties": false
            },
            "tool_support": {
                "type": "object",
                "required": ["native", "text", "tool_search"],
                "properties": {
                    "native": {"type": "boolean"},
                    "text": {"type": "boolean"},
                    "preferred_format": {"type": "string"},
                    "parity": {"type": "string"},
                    "parity_notes": {"type": "string"},
                    "empirical_parity": {"$ref": "#/$defs/tool_empirical_parity"},
                    "tool_search": {"type": "array", "items": {"type": "string"}},
                    "max_tools": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            },
            "tool_empirical_parity": {
                "type": "object",
                "required": [
                    "verdict",
                    "preferred_format",
                    "confidence",
                    "sample_size",
                    "last_evaluated",
                    "native_pass_rate",
                    "text_pass_rate",
                    "verifier_divergence_rate"
                ],
                "properties": {
                    "verdict": {"type": "string"},
                    "preferred_format": {"type": "string"},
                    "confidence": {"type": "string"},
                    "sample_size": {"type": "integer", "minimum": 1},
                    "last_evaluated": {"type": "string", "minLength": 1},
                    "native_pass_rate": {"type": "number", "minimum": 0, "maximum": 1},
                    "text_pass_rate": {"type": "number", "minimum": 0, "maximum": 1},
                    "verifier_divergence_rate": {"type": "number", "minimum": 0, "maximum": 1}
                },
                "additionalProperties": false
            },
            "format_preferences": {
                "type": "object",
                "required": [
                    "prefers_xml_scaffolding",
                    "prefers_markdown_scaffolding",
                    "structured_output_mode",
                    "supports_assistant_prefill",
                    "prefers_role_developer",
                    "prefers_xml_tools",
                    "thinking_block_style"
                ],
                "properties": {
                    "prefers_xml_scaffolding": {"type": "boolean"},
                    "prefers_markdown_scaffolding": {"type": "boolean"},
                    "structured_output_mode": {"enum": ["native_json", "delimited", "xml_tagged", "none"]},
                    "supports_assistant_prefill": {"type": "boolean"},
                    "prefers_role_developer": {"type": "boolean"},
                    "prefers_xml_tools": {"type": "boolean"},
                    "thinking_block_style": {"enum": ["none", "thinking_blocks", "reasoning_summary", "inline"]}
                },
                "additionalProperties": false
            },
            "reasoning": {
                "type": "object",
                "required": ["modes", "effort_supported", "none_supported", "interleaved_supported", "preserve_thinking"],
                "properties": {
                    "modes": {"type": "array", "items": {"type": "string"}},
                    "effort_supported": {"type": "boolean"},
                    "none_supported": {"type": "boolean"},
                    "interleaved_supported": {"type": "boolean"},
                    "preserve_thinking": {"type": "boolean"}
                },
                "additionalProperties": false
            },
            "pricing": {
                "type": "object",
                "required": ["input_per_mtok", "output_per_mtok"],
                "properties": {
                    "input_per_mtok": {"type": "number", "minimum": 0},
                    "output_per_mtok": {"type": "number", "minimum": 0},
                    "cache_read_per_mtok": {"type": ["number", "null"], "minimum": 0},
                    "cache_write_per_mtok": {"type": ["number", "null"], "minimum": 0}
                },
                "additionalProperties": false
            },
            "fast_mode": {
                "type": "object",
                "required": ["param", "value"],
                "properties": {
                    "param": {"type": "string", "minLength": 1},
                    "value": {"type": "string", "minLength": 1},
                    "beta_header": {"type": "string"},
                    "otps_speedup": {"type": "number", "exclusiveMinimum": 0},
                    "status": {"type": "string"},
                    "pricing": {"$ref": "#/$defs/pricing"},
                    "note": {"type": "string"}
                },
                "additionalProperties": false
            },
            "deprecation": {
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": {"enum": ["active", "deprecated"]},
                    "note": {"type": "string"},
                    "superseded_by": {"type": "string"}
                },
                "additionalProperties": false
            },
            "variant": {
                "type": "object",
                "required": ["id", "label", "description", "model_id", "provider", "source"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "label": {"type": "string", "minLength": 1},
                    "description": {"type": "string"},
                    "model_id": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "source": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            }
        }
    })
}

fn catalog_provider(id: String, provider: ProviderDef) -> CatalogProvider {
    CatalogProvider {
        display_name: provider
            .display_name
            .clone()
            .unwrap_or_else(|| title_case(&id)),
        icon: provider.icon.clone(),
        classification: provider_classification(&provider),
        endpoint: ProviderEndpoint {
            base_url: provider.base_url.clone(),
            base_url_env: provider.base_url_env.clone(),
            chat_endpoint: provider.chat_endpoint.clone(),
            completion_endpoint: provider.completion_endpoint.clone(),
        },
        auth: ProviderAuth {
            style: provider.auth_style.clone(),
            header: provider.auth_header.clone(),
            env: llm_config::auth_env_names(&provider.auth_env),
            required: provider.auth_style != "none",
        },
        protocols: provider_protocols(&id, &provider),
        features: provider.features.clone(),
        caveats: provider_caveats(&id, &provider),
        rpm: provider.rpm,
        latency_p50_ms: provider.latency_p50_ms,
        id,
    }
}

fn catalog_alias(name: &str, alias: &AliasDef) -> CatalogAlias {
    CatalogAlias {
        name: name.to_string(),
        model_id: alias.id.clone(),
        provider: alias.provider.clone(),
        tool_format: alias.tool_format.clone(),
        tool_calling: llm_config::alias_tool_calling_entry(name),
    }
}

fn catalog_model(
    id: String,
    model: ModelDef,
    aliases_by_model: &BTreeMap<(String, String), Vec<String>>,
) -> CatalogModel {
    let caps = llm::capabilities::lookup(&model.provider, &id);
    let structured_output = caps
        .structured_output
        .clone()
        .or_else(|| caps.json_schema.clone())
        .unwrap_or_else(|| "none".to_string());
    let aliases = aliases_by_model
        .get(&(model.provider.clone(), id.clone()))
        .cloned()
        .unwrap_or_default();
    let quality_tags = model_quality_tags(&model, &aliases);
    CatalogModel {
        aliases,
        modalities: modalities_from_caps(&caps),
        tool_support: ModelToolSupport {
            native: caps.native_tools,
            text: caps.text_tool_wire_format_supported,
            preferred_format: caps.preferred_tool_format.clone(),
            parity: caps.tool_mode_parity.clone(),
            parity_notes: caps.tool_mode_parity_notes.clone(),
            empirical_parity: None,
            tool_search: caps.tool_search.clone(),
            max_tools: caps.max_tools,
        },
        structured_output,
        format_preferences: ModelFormatPreferences {
            prefers_xml_scaffolding: caps.prefers_xml_scaffolding,
            prefers_markdown_scaffolding: caps.prefers_markdown_scaffolding,
            structured_output_mode: caps.structured_output_mode.clone(),
            supports_assistant_prefill: caps.supports_assistant_prefill,
            prefers_role_developer: caps.prefers_role_developer,
            prefers_xml_tools: caps.prefers_xml_tools,
            thinking_block_style: caps.thinking_block_style.clone(),
        },
        reasoning: ModelReasoning {
            modes: caps.thinking_modes.clone(),
            effort_supported: caps.reasoning_effort_supported,
            none_supported: caps.reasoning_none_supported,
            interleaved_supported: caps.interleaved_thinking_supported,
            preserve_thinking: caps.preserve_thinking,
        },
        prompt_cache: caps.prompt_caching,
        pricing: model.pricing.clone(),
        deprecation: ModelDeprecation {
            status: if model.deprecated {
                DeprecationStatus::Deprecated
            } else {
                DeprecationStatus::Active
            },
            note: model.deprecation_note.clone(),
            superseded_by: model.superseded_by.clone(),
        },
        availability: ModelAvailabilityStatus::from(model.availability),
        quality_tags,
        capability_tags: model.capabilities.clone(),
        family: llm_config::model_family(&model.provider, &id),
        lineage: llm_config::model_lineage(&model.provider, &id),
        complementary_with: model.complementary_with.clone(),
        avoid_as_reviewer_for: model.avoid_as_reviewer_for.clone(),
        tier: llm_config::model_tier(&id),
        open_weight: model.open_weight,
        strengths: model.strengths.clone(),
        benchmarks: model.benchmarks.clone(),
        fast_mode: model.fast_mode.as_ref().map(|fm| ModelFastMode {
            param: fm.param.clone(),
            value: fm.value.clone(),
            beta_header: fm.beta_header.clone(),
            otps_speedup: fm.otps_speedup,
            status: fm.status.clone(),
            pricing: fm.pricing.clone(),
            note: fm.note.clone(),
        }),
        id,
        name: model.name,
        provider: model.provider,
        context_window: model.context_window,
        runtime_context_window: model.runtime_context_window,
        stream_timeout: model.stream_timeout,
    }
}

fn model_quality_tags(model: &ModelDef, aliases: &[String]) -> Vec<String> {
    let mut tags: BTreeSet<String> = model.quality_tags.iter().cloned().collect();
    for alias in aliases {
        match alias.as_str() {
            "frontier" | "tier/frontier" => {
                tags.insert("frontier".to_string());
            }
            "mid" | "tier/mid" => {
                tags.insert("balanced".to_string());
            }
            "small" | "tier/small" => {
                tags.insert("small".to_string());
            }
            _ => {}
        }
    }
    if is_local_provider(&model.provider) {
        tags.insert("local".to_string());
    }
    tags.into_iter().collect()
}

fn aliases_by_model(aliases: &[(String, AliasDef)]) -> BTreeMap<(String, String), Vec<String>> {
    let mut by_model: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (name, alias) in aliases {
        by_model
            .entry((alias.provider.clone(), alias.id.clone()))
            .or_default()
            .push(name.clone());
    }
    for names in by_model.values_mut() {
        names.sort();
    }
    by_model
}

fn modalities_from_caps(caps: &llm::capabilities::Capabilities) -> ModelModalities {
    let mut input = vec!["text".to_string()];
    if caps.vision || caps.vision_supported {
        input.push("image".to_string());
    }
    if caps.audio {
        input.push("audio".to_string());
    }
    if caps.pdf {
        input.push("pdf".to_string());
    }
    ModelModalities {
        input,
        output: vec!["text".to_string()],
    }
}

fn catalog_variants(models: &[CatalogModel], aliases: &[CatalogAlias]) -> Vec<CatalogVariant> {
    let mut variants = Vec::new();
    for (id, label, description, alias_name) in [
        (
            "fast",
            "Fast",
            "Lowest-latency general coding-agent route.",
            "small",
        ),
        (
            "balanced",
            "Balanced",
            "Default cost/quality tradeoff for routine coding-agent work.",
            "mid",
        ),
        (
            "high-reasoning",
            "High reasoning",
            "Frontier route for hard planning, repair, and review tasks.",
            "frontier",
        ),
    ] {
        if let Some(alias) = aliases.iter().find(|alias| alias.name == alias_name) {
            variants.push(CatalogVariant {
                id: id.to_string(),
                label: label.to_string(),
                description: description.to_string(),
                model_id: alias.model_id.clone(),
                provider: alias.provider.clone(),
                source: format!("alias:{alias_name}"),
            });
        }
    }
    push_variant_from_model(
        &mut variants,
        "local",
        "Local",
        "Best local/offline model route in the checked-in catalog.",
        models
            .iter()
            .filter(|model| is_local_provider(&model.provider))
            .max_by_key(|model| model.context_window),
    );
    push_variant_from_model(
        &mut variants,
        "cheap",
        "Cheap",
        "Lowest known hosted input+output token price.",
        models
            .iter()
            .filter(|model| !is_local_provider(&model.provider))
            .min_by(|left, right| {
                pricing_total(left)
                    .partial_cmp(&pricing_total(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
    );
    push_variant_from_model(
        &mut variants,
        "vision-capable",
        "Vision capable",
        "A model route that accepts image input.",
        models
            .iter()
            .filter(|model| model.modalities.input.iter().any(|mode| mode == "image"))
            .max_by_key(|model| model.context_window),
    );
    push_variant_from_model(
        &mut variants,
        "long-context",
        "Long context",
        "Largest context-window route in the checked-in catalog.",
        models.iter().max_by_key(|model| model.context_window),
    );
    variants
}

fn push_variant_from_model(
    variants: &mut Vec<CatalogVariant>,
    id: &str,
    label: &str,
    description: &str,
    model: Option<&CatalogModel>,
) {
    if let Some(model) = model {
        variants.push(CatalogVariant {
            id: id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            model_id: model.id.clone(),
            provider: model.provider.clone(),
            source: "catalog".to_string(),
        });
    }
}

fn pricing_total(model: &CatalogModel) -> f64 {
    model
        .pricing
        .as_ref()
        .map(|pricing| pricing.input_per_mtok + pricing.output_per_mtok)
        .unwrap_or(f64::MAX)
}

fn validate_pricing(
    model: &CatalogModel,
    pricing: &ModelPricing,
    result: &mut ProviderCatalogValidation,
) {
    for (field, value) in [
        ("input_per_mtok", Some(pricing.input_per_mtok)),
        ("output_per_mtok", Some(pricing.output_per_mtok)),
        ("cache_read_per_mtok", pricing.cache_read_per_mtok),
        ("cache_write_per_mtok", pricing.cache_write_per_mtok),
    ] {
        if value.is_some_and(|value| value < 0.0) {
            result.errors.push(format!(
                "model {} pricing.{} must be non-negative",
                model.id, field
            ));
        }
    }
}

fn validate_token_field(
    model: &CatalogModel,
    field: &str,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if !is_catalog_token(value) {
        result.errors.push(format!(
            "model {} {field} must be a lowercase catalog token, got {:?}",
            model.id, value
        ));
    }
}

fn validate_reviewer_selector(
    model: &CatalogModel,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if value.trim().is_empty() {
        result.errors.push(format!(
            "model {} avoid_as_reviewer_for cannot contain an empty selector",
            model.id
        ));
    }
}

fn is_catalog_token(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn provider_classification(provider: &ProviderDef) -> ProviderClassification {
    if provider.auth_style == "none"
        || provider.base_url.contains("localhost")
        || provider.base_url.contains("127.0.0.1")
    {
        ProviderClassification::Local
    } else {
        ProviderClassification::Hosted
    }
}

fn provider_protocols(id: &str, provider: &ProviderDef) -> Vec<String> {
    match id {
        "anthropic" => vec!["anthropic_messages".to_string()],
        "gemini" => vec!["gemini_generate_content".to_string()],
        "vertex" => vec!["vertex_generate_content".to_string()],
        "bedrock" => vec!["bedrock_converse".to_string()],
        "azure_openai" => vec!["azure_openai_chat_completions".to_string()],
        "ollama" if provider.chat_endpoint.starts_with("/api/") => {
            vec!["ollama_native".to_string()]
        }
        _ => vec!["openai_chat_completions".to_string()],
    }
}

fn provider_caveats(id: &str, provider: &ProviderDef) -> Vec<String> {
    let mut caveats = Vec::new();
    if provider.auth_style == "aws_sigv4" {
        caveats.push("Credentials are resolved through the AWS SDK chain.".to_string());
    }
    if id == "azure_openai" {
        caveats.push("The Harn model field names the Azure deployment.".to_string());
    }
    if id == "ollama" && provider.chat_endpoint == "/api/chat" {
        caveats.push(
            "Native Ollama chat returns NDJSON and can apply model-family parsers.".to_string(),
        );
    }
    caveats
}

fn is_local_provider(provider: &str) -> bool {
    matches!(
        provider,
        "ollama" | "local" | "llamacpp" | "mlx" | "vllm" | "tgi"
    )
}

fn is_tier_alias(name: &str) -> bool {
    matches!(
        name,
        "frontier"
            | "mid"
            | "small"
            | "tier/frontier"
            | "tier/mid"
            | "tier/small"
            | "sonnet"
            | "opus"
            | "haiku"
    )
}

fn title_case(id: &str) -> String {
    id.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn generated_header(comment: &str, language: &str) -> String {
    format!(
        "{comment} GENERATED by `{PROVIDER_CATALOG_GENERATOR}` - do not edit by hand.\n{comment} Source: Harn runtime provider catalog schema v{PROVIDER_CATALOG_SCHEMA_VERSION}.\n{comment} Language: {language}.\n\n"
    )
}

const TYPESCRIPT_TYPES: &str = r#"export interface HarnProviderCatalog {
  schema_version: 2
  schema: string
  generated_by: string
  providers: HarnCatalogProvider[]
  models: HarnCatalogModel[]
  aliases: HarnCatalogAlias[]
  variants: HarnCatalogVariant[]
  qc_defaults: Record<string, string>
}

export interface HarnCatalogProvider {
  id: string
  display_name: string
  icon?: string
  classification: "hosted" | "local"
  endpoint: HarnProviderEndpoint
  auth: HarnProviderAuth
  protocols: string[]
  features: string[]
  caveats: string[]
  rpm?: number
  latency_p50_ms?: number
}

export interface HarnProviderEndpoint {
  base_url: string
  base_url_env?: string
  chat_endpoint: string
  completion_endpoint?: string
}

export interface HarnProviderAuth {
  style: string
  header?: string
  env: string[]
  required: boolean
}

export interface HarnCatalogAlias {
  name: string
  model_id: string
  provider: string
  tool_format?: string
  tool_calling?: HarnAliasToolCalling
}

export interface HarnAliasToolCalling {
  native?: string
  text?: string
  streaming_native?: string
  fallback_mode?: string
  failure_reason?: string
  last_probe_at?: string
}

export interface HarnCatalogModel {
  id: string
  name: string
  provider: string
  aliases: string[]
  context_window: number
  runtime_context_window?: number
  stream_timeout?: number
  modalities: { input: string[]; output: string[] }
  tool_support: {
    native: boolean
    text: boolean
    preferred_format?: string
    parity?: string
    parity_notes?: string
    empirical_parity?: HarnToolEmpiricalParity
    tool_search: string[]
    max_tools?: number
  }
  structured_output: string
  format_preferences: {
    prefers_xml_scaffolding: boolean
    prefers_markdown_scaffolding: boolean
    structured_output_mode: "native_json" | "delimited" | "xml_tagged" | "none"
    supports_assistant_prefill: boolean
    prefers_role_developer: boolean
    prefers_xml_tools: boolean
    thinking_block_style: "none" | "thinking_blocks" | "reasoning_summary" | "inline"
  }
  reasoning: {
    modes: string[]
    effort_supported: boolean
    none_supported: boolean
    interleaved_supported: boolean
    preserve_thinking: boolean
  }
  prompt_cache: boolean
  pricing?: HarnModelPricing
  deprecation: { status: "active" | "deprecated"; note?: string; superseded_by?: string }
  availability: "serverless" | "dedicated" | "unknown"
  quality_tags: string[]
  capability_tags: string[]
  family: string
  lineage: string
  complementary_with?: string[]
  avoid_as_reviewer_for?: string[]
  tier: "small" | "mid" | "frontier" | "reasoning"
  open_weight?: boolean
  strengths?: string[]
  benchmarks?: Record<string, number>
  fast_mode?: HarnModelFastMode
}

export interface HarnToolEmpiricalParity {
  verdict: string
  preferred_format: string
  confidence: string
  sample_size: number
  last_evaluated: string
  native_pass_rate: number
  text_pass_rate: number
  verifier_divergence_rate: number
}

export interface HarnModelPricing {
  input_per_mtok: number
  output_per_mtok: number
  cache_read_per_mtok?: number | null
  cache_write_per_mtok?: number | null
}

export interface HarnModelFastMode {
  param: string
  value: string
  beta_header?: string
  otps_speedup?: number
  status?: string
  pricing?: HarnModelPricing
  note?: string
}

export interface HarnCatalogVariant {
  id: string
  label: string
  description: string
  model_id: string
  provider: string
  source: string
}

export interface CatalogEntry {
  id: string
  name: string
  provider: string
  contextWindow: number
  runtimeContextWindow?: number
  capabilities: string[]
  family: string
  lineage: string
  pricing?: {
    inputPerMTok: number
    outputPerMTok: number
    cacheReadPerMTok?: number | null
    cacheWritePerMTok?: number | null
  }
  streamTimeout?: number
}

export interface CatalogAlias {
  alias: string
  id: string
  provider: string
  toolFormat?: string
  toolCalling?: HarnAliasToolCalling
}

"#;

const TYPESCRIPT_COMPAT_EXPORTS: &str = r#"
export const MODEL_CATALOG: readonly CatalogEntry[] = harnProviderCatalog.models.map((model) => ({
  id: model.id,
  name: model.name,
  provider: model.provider,
  contextWindow: model.context_window,
  runtimeContextWindow: model.runtime_context_window,
  capabilities: model.capability_tags,
  family: model.family,
  lineage: model.lineage,
  pricing: model.pricing
    ? {
        inputPerMTok: model.pricing.input_per_mtok,
        outputPerMTok: model.pricing.output_per_mtok,
        cacheReadPerMTok: model.pricing.cache_read_per_mtok,
        cacheWritePerMTok: model.pricing.cache_write_per_mtok,
      }
    : undefined,
  streamTimeout: model.stream_timeout,
}))

export const ALIASES: readonly CatalogAlias[] = harnProviderCatalog.aliases.map((alias) => ({
  alias: alias.name,
  id: alias.model_id,
  provider: alias.provider,
  toolFormat: alias.tool_format,
  toolCalling: alias.tool_calling,
}))

export const QC_DEFAULTS: Readonly<Record<string, string>> = harnProviderCatalog.qc_defaults

export function pricingFor(modelId: string): CatalogEntry["pricing"] | undefined {
  return entryFor(modelId)?.pricing
}

export function entryFor(modelId: string): CatalogEntry | undefined {
  return MODEL_CATALOG.find((entry) => entry.id === modelId)
}

export function aliasesByProvider(provider: string): readonly CatalogAlias[] {
  return ALIASES.filter((alias) => alias.provider === provider)
}

export function qcDefaultModel(provider: string): string | undefined {
  return QC_DEFAULTS[provider]
}
"#;

const SWIFT_TYPES: &str = r#"public struct HarnProviderCatalog: Codable, Sendable, Equatable {
    public let schemaVersion: Int
    public let schema: String
    public let generatedBy: String
    public let providers: [HarnCatalogProvider]
    public let models: [HarnCatalogModel]
    public let aliases: [HarnCatalogAlias]
    public let variants: [HarnCatalogVariant]
    public let qcDefaults: [String: String]

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case schema
        case generatedBy = "generated_by"
        case providers
        case models
        case aliases
        case variants
        case qcDefaults = "qc_defaults"
    }
}

public struct HarnCatalogProvider: Codable, Sendable, Equatable {
    public let id: String
    public let displayName: String
    public let icon: String?
    public let classification: String
    public let endpoint: HarnProviderEndpoint
    public let auth: HarnProviderAuth
    public let protocols: [String]
    public let features: [String]
    public let caveats: [String]
    public let rpm: Int?
    public let latencyP50Ms: Int?

    enum CodingKeys: String, CodingKey {
        case id
        case displayName = "display_name"
        case icon
        case classification
        case endpoint
        case auth
        case protocols
        case features
        case caveats
        case rpm
        case latencyP50Ms = "latency_p50_ms"
    }
}

public struct HarnProviderEndpoint: Codable, Sendable, Equatable {
    public let baseURL: String
    public let baseURLEnv: String?
    public let chatEndpoint: String
    public let completionEndpoint: String?

    enum CodingKeys: String, CodingKey {
        case baseURL = "base_url"
        case baseURLEnv = "base_url_env"
        case chatEndpoint = "chat_endpoint"
        case completionEndpoint = "completion_endpoint"
    }
}

public struct HarnProviderAuth: Codable, Sendable, Equatable {
    public let style: String
    public let header: String?
    public let env: [String]
    public let required: Bool
}

public struct HarnCatalogAlias: Codable, Sendable, Equatable {
    public let name: String
    public let modelID: String
    public let provider: String
    public let toolFormat: String?
    public let toolCalling: HarnAliasToolCalling?

    enum CodingKeys: String, CodingKey {
        case name
        case modelID = "model_id"
        case provider
        case toolFormat = "tool_format"
        case toolCalling = "tool_calling"
    }
}

public struct HarnAliasToolCalling: Codable, Sendable, Equatable {
    public let native: String?
    public let text: String?
    public let streamingNative: String?
    public let fallbackMode: String?
    public let failureReason: String?
    public let lastProbeAt: String?

    enum CodingKeys: String, CodingKey {
        case native
        case text
        case streamingNative = "streaming_native"
        case fallbackMode = "fallback_mode"
        case failureReason = "failure_reason"
        case lastProbeAt = "last_probe_at"
    }
}

public struct HarnCatalogModel: Codable, Sendable, Equatable {
    public let id: String
    public let name: String
    public let provider: String
    public let aliases: [String]
    public let contextWindow: Int
    public let runtimeContextWindow: Int?
    public let streamTimeout: Double?
    public let modalities: HarnModelModalities
    public let toolSupport: HarnModelToolSupport
    public let structuredOutput: String
    public let formatPreferences: HarnModelFormatPreferences
    public let reasoning: HarnModelReasoning
    public let promptCache: Bool
    public let pricing: HarnModelPricing?
    public let deprecation: HarnModelDeprecation
    public let availability: String
    public let qualityTags: [String]
    public let capabilityTags: [String]
    public let family: String
    public let lineage: String
    public let complementaryWith: [String]
    public let avoidAsReviewerFor: [String]
    /// Popular-consensus tier label: "small" | "mid" | "frontier" | "reasoning".
    public let tier: String
    /// True when weights are downloadable / self-hostable; nil when the
    /// catalog row predates the field.
    public let openWeight: Bool?
    /// Workload-shaped strength tags (`coding`, `summarization`, `vision`, ...).
    public let strengths: [String]
    /// Public benchmark numbers keyed by `snake_case` identifier.
    public let benchmarks: [String: Double]
    /// Accelerated-serving ("fast mode") tier metadata, when offered.
    public let fastMode: HarnModelFastMode?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case provider
        case aliases
        case contextWindow = "context_window"
        case runtimeContextWindow = "runtime_context_window"
        case streamTimeout = "stream_timeout"
        case modalities
        case toolSupport = "tool_support"
        case structuredOutput = "structured_output"
        case formatPreferences = "format_preferences"
        case reasoning
        case promptCache = "prompt_cache"
        case pricing
        case deprecation
        case availability
        case qualityTags = "quality_tags"
        case capabilityTags = "capability_tags"
        case family
        case lineage
        case complementaryWith = "complementary_with"
        case avoidAsReviewerFor = "avoid_as_reviewer_for"
        case tier
        case openWeight = "open_weight"
        case strengths
        case benchmarks
        case fastMode = "fast_mode"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        provider = try container.decode(String.self, forKey: .provider)
        aliases = try container.decode([String].self, forKey: .aliases)
        contextWindow = try container.decode(Int.self, forKey: .contextWindow)
        runtimeContextWindow = try container.decodeIfPresent(Int.self, forKey: .runtimeContextWindow)
        streamTimeout = try container.decodeIfPresent(Double.self, forKey: .streamTimeout)
        modalities = try container.decode(HarnModelModalities.self, forKey: .modalities)
        toolSupport = try container.decode(HarnModelToolSupport.self, forKey: .toolSupport)
        structuredOutput = try container.decode(String.self, forKey: .structuredOutput)
        formatPreferences = try container.decode(HarnModelFormatPreferences.self, forKey: .formatPreferences)
        reasoning = try container.decode(HarnModelReasoning.self, forKey: .reasoning)
        promptCache = try container.decode(Bool.self, forKey: .promptCache)
        pricing = try container.decodeIfPresent(HarnModelPricing.self, forKey: .pricing)
        deprecation = try container.decode(HarnModelDeprecation.self, forKey: .deprecation)
        availability = try container.decode(String.self, forKey: .availability)
        qualityTags = try container.decode([String].self, forKey: .qualityTags)
        capabilityTags = try container.decode([String].self, forKey: .capabilityTags)
        family = try container.decode(String.self, forKey: .family)
        lineage = try container.decode(String.self, forKey: .lineage)
        complementaryWith = try container.decodeIfPresent([String].self, forKey: .complementaryWith) ?? []
        avoidAsReviewerFor = try container.decodeIfPresent([String].self, forKey: .avoidAsReviewerFor) ?? []
        tier = try container.decode(String.self, forKey: .tier)
        openWeight = try container.decodeIfPresent(Bool.self, forKey: .openWeight)
        strengths = try container.decodeIfPresent([String].self, forKey: .strengths) ?? []
        benchmarks = try container.decodeIfPresent([String: Double].self, forKey: .benchmarks) ?? [:]
        fastMode = try container.decodeIfPresent(HarnModelFastMode.self, forKey: .fastMode)
    }
}

public struct HarnModelModalities: Codable, Sendable, Equatable {
    public let input: [String]
    public let output: [String]
}

public struct HarnModelToolSupport: Codable, Sendable, Equatable {
    public let native: Bool
    public let text: Bool
    public let preferredFormat: String?
    public let parity: String?
    public let parityNotes: String?
    public let empiricalParity: HarnToolEmpiricalParity?
    public let toolSearch: [String]
    public let maxTools: Int?

    enum CodingKeys: String, CodingKey {
        case native
        case text
        case preferredFormat = "preferred_format"
        case parity
        case parityNotes = "parity_notes"
        case empiricalParity = "empirical_parity"
        case toolSearch = "tool_search"
        case maxTools = "max_tools"
    }
}

public struct HarnToolEmpiricalParity: Codable, Sendable, Equatable {
    public let verdict: String
    public let preferredFormat: String
    public let confidence: String
    public let sampleSize: Int
    public let lastEvaluated: String
    public let nativePassRate: Double
    public let textPassRate: Double
    public let verifierDivergenceRate: Double

    enum CodingKeys: String, CodingKey {
        case verdict
        case preferredFormat = "preferred_format"
        case confidence
        case sampleSize = "sample_size"
        case lastEvaluated = "last_evaluated"
        case nativePassRate = "native_pass_rate"
        case textPassRate = "text_pass_rate"
        case verifierDivergenceRate = "verifier_divergence_rate"
    }
}

public struct HarnModelFormatPreferences: Codable, Sendable, Equatable {
    public let prefersXMLScaffolding: Bool
    public let prefersMarkdownScaffolding: Bool
    public let structuredOutputMode: String
    public let supportsAssistantPrefill: Bool
    public let prefersRoleDeveloper: Bool
    public let prefersXMLTools: Bool
    public let thinkingBlockStyle: String

    enum CodingKeys: String, CodingKey {
        case prefersXMLScaffolding = "prefers_xml_scaffolding"
        case prefersMarkdownScaffolding = "prefers_markdown_scaffolding"
        case structuredOutputMode = "structured_output_mode"
        case supportsAssistantPrefill = "supports_assistant_prefill"
        case prefersRoleDeveloper = "prefers_role_developer"
        case prefersXMLTools = "prefers_xml_tools"
        case thinkingBlockStyle = "thinking_block_style"
    }
}

public struct HarnModelReasoning: Codable, Sendable, Equatable {
    public let modes: [String]
    public let effortSupported: Bool
    public let noneSupported: Bool
    public let interleavedSupported: Bool
    public let preserveThinking: Bool

    enum CodingKeys: String, CodingKey {
        case modes
        case effortSupported = "effort_supported"
        case noneSupported = "none_supported"
        case interleavedSupported = "interleaved_supported"
        case preserveThinking = "preserve_thinking"
    }
}

public struct HarnModelPricing: Codable, Sendable, Equatable {
    public let inputPerMTok: Double
    public let outputPerMTok: Double
    public let cacheReadPerMTok: Double?
    public let cacheWritePerMTok: Double?

    enum CodingKeys: String, CodingKey {
        case inputPerMTok = "input_per_mtok"
        case outputPerMTok = "output_per_mtok"
        case cacheReadPerMTok = "cache_read_per_mtok"
        case cacheWritePerMTok = "cache_write_per_mtok"
    }
}

public struct HarnModelDeprecation: Codable, Sendable, Equatable {
    public let status: String
    public let note: String?
    public let supersededBy: String?

    enum CodingKeys: String, CodingKey {
        case status
        case note
        case supersededBy = "superseded_by"
    }
}

public struct HarnModelFastMode: Codable, Sendable, Equatable {
    public let param: String
    public let value: String
    public let betaHeader: String?
    public let otpsSpeedup: Double?
    public let status: String?
    public let pricing: HarnModelPricing?
    public let note: String?

    enum CodingKeys: String, CodingKey {
        case param
        case value
        case betaHeader = "beta_header"
        case otpsSpeedup = "otps_speedup"
        case status
        case pricing
        case note
    }
}

public struct HarnCatalogVariant: Codable, Sendable, Equatable {
    public let id: String
    public let label: String
    public let description: String
    public let modelID: String
    public let provider: String
    public let source: String

    enum CodingKeys: String, CodingKey {
        case id
        case label
        case description
        case modelID = "model_id"
        case provider
        case source
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, MutexGuard};

    static RUNTIME_REFRESH_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct OverrideGuard;

    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            llm_config::clear_user_overrides();
        }
    }

    struct RuntimeCatalogGuard {
        _lock: MutexGuard<'static, ()>,
        _runtime_paths_env_lock: MutexGuard<'static, ()>,
        state_dir: tempfile::TempDir,
        previous_state_dir: Option<String>,
        previous_allow_unsigned: Option<String>,
        previous_disable_refresh: Option<String>,
        previous_trusted_keys: Option<String>,
    }

    impl RuntimeCatalogGuard {
        fn new() -> Self {
            let lock = RUNTIME_REFRESH_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let runtime_paths_env_lock = crate::runtime_paths::test_env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let state_dir = tempfile::tempdir().expect("temp state dir");
            let previous_state_dir = std::env::var(crate::runtime_paths::HARN_STATE_DIR_ENV).ok();
            let previous_allow_unsigned =
                std::env::var(HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV).ok();
            let previous_disable_refresh = std::env::var(HARN_DISABLE_CATALOG_REFRESH_ENV).ok();
            let previous_trusted_keys = std::env::var(HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV).ok();
            unsafe {
                std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, state_dir.path());
                std::env::remove_var(HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV);
                std::env::remove_var(HARN_DISABLE_CATALOG_REFRESH_ENV);
                std::env::remove_var(HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV);
            }
            llm_config::clear_runtime_catalog_overlay();
            Self {
                _lock: lock,
                _runtime_paths_env_lock: runtime_paths_env_lock,
                state_dir,
                previous_state_dir,
                previous_allow_unsigned,
                previous_disable_refresh,
                previous_trusted_keys,
            }
        }
    }

    impl Drop for RuntimeCatalogGuard {
        fn drop(&mut self) {
            llm_config::clear_runtime_catalog_overlay();
            match self.previous_state_dir.as_deref() {
                Some(value) => unsafe {
                    std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, value);
                },
                None => unsafe { std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV) },
            }
            restore_env_var(
                HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV,
                self.previous_allow_unsigned.as_deref(),
            );
            restore_env_var(
                HARN_DISABLE_CATALOG_REFRESH_ENV,
                self.previous_disable_refresh.as_deref(),
            );
            restore_env_var(
                HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV,
                self.previous_trusted_keys.as_deref(),
            );
        }
    }

    fn restore_env_var(name: &str, value: Option<&str>) {
        match value {
            Some(value) => unsafe { std::env::set_var(name, value) },
            None => unsafe { std::env::remove_var(name) },
        }
    }

    fn install_overlay(toml_src: &str) -> OverrideGuard {
        let overlay = llm_config::parse_config_toml(toml_src).expect("overlay parses");
        llm_config::set_user_overrides(Some(overlay));
        OverrideGuard
    }

    fn remote_catalog_with_extra_model() -> ProviderCatalogArtifact {
        let mut remote = artifact();
        let mut provider = remote.providers[0].clone();
        provider.id = "refreshco".to_string();
        provider.display_name = "Refresh Co".to_string();
        provider.endpoint.base_url = "https://refresh.example/v1".to_string();
        provider.auth.style = "none".to_string();
        provider.auth.required = false;
        provider.auth.env.clear();
        remote.providers.push(provider);

        let mut model = remote.models[0].clone();
        model.id = "refreshco/new-model".to_string();
        model.name = "Refresh Co New Model".to_string();
        model.provider = "refreshco".to_string();
        model.aliases = vec!["refresh-new".to_string()];
        model.context_window = 123_456;
        model.deprecation.status = DeprecationStatus::Active;
        model.deprecation.note = None;
        model.deprecation.superseded_by = None;
        remote.models.push(model);

        remote.aliases.push(CatalogAlias {
            name: "refresh-new".to_string(),
            model_id: "refreshco/new-model".to_string(),
            provider: "refreshco".to_string(),
            tool_format: Some("text".to_string()),
            tool_calling: None,
        });
        remote
    }

    fn spawn_catalog_stub(body: String) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind catalog stub");
        let url = format!("http://{}/catalog.json", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept catalog request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\netag: \"fixture-v1\"\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write catalog response");
        });
        (url, handle)
    }

    #[test]
    fn generated_catalog_validates() {
        llm_config::clear_user_overrides();
        let report = validate_current();
        assert!(
            report.errors.is_empty(),
            "catalog validation errors: {:?}",
            report.errors
        );
    }

    #[tokio::test]
    async fn runtime_refresh_installs_valid_remote_catalog_overlay() {
        let guard = RuntimeCatalogGuard::new();
        let remote = remote_catalog_with_extra_model();
        let body = serde_json::to_string(&remote).expect("remote catalog serializes");
        let (url, server) = spawn_catalog_stub(body);

        let report = refresh_runtime_catalog(CatalogRefreshOptions {
            url: Some(url),
            force: true,
        })
        .await;
        server.join().expect("catalog server exits");

        assert_eq!(report.status, "refreshed");
        assert!(report.refreshed);
        assert_eq!(report.etag.as_deref(), Some("\"fixture-v1\""));
        assert!(guard
            .state_dir
            .path()
            .join("cache/provider-catalog/catalog.json")
            .is_file());

        let refreshed = llm_config::model_catalog_entry("refreshco/new-model")
            .expect("remote model installed into runtime catalog");
        assert_eq!(refreshed.name, "Refresh Co New Model");
        assert_eq!(refreshed.context_window, 123_456);
        assert!(llm_config::known_model_names()
            .iter()
            .any(|name| name == "refresh-new"));
    }

    #[tokio::test]
    async fn runtime_refresh_rejects_malformed_remote_without_emptying_catalog() {
        let _guard = RuntimeCatalogGuard::new();
        let baseline_count = llm_config::model_catalog_entries().len();
        let (url, server) = spawn_catalog_stub(r#"{"schema_version":2,"models":[]}"#.to_string());

        let report = refresh_runtime_catalog(CatalogRefreshOptions {
            url: Some(url),
            force: true,
        })
        .await;
        server.join().expect("catalog server exits");

        assert_eq!(report.status, "fallback");
        assert!(report.warning.as_deref().is_some_and(|warning| {
            warning.contains("catalog JSON does not match")
                || warning.contains("catalog has no providers")
                || warning.contains("unsigned")
        }));
        assert_eq!(llm_config::model_catalog_entries().len(), baseline_count);
    }

    #[test]
    fn signed_catalog_envelope_accepts_trusted_key() {
        let _guard = RuntimeCatalogGuard::new();
        let catalog = remote_catalog_with_extra_model();
        let signing_key = SigningKey::from_bytes(&[42; 32]);
        let canonical = serde_json::to_vec(&catalog).expect("catalog canonicalizes");
        let signature = signing_key.sign(&canonical);
        let public_key = base64::engine::general_purpose::STANDARD
            .encode(signing_key.verifying_key().to_bytes());
        unsafe {
            std::env::set_var(
                HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV,
                format!("test={public_key}"),
            );
        }
        let document = json!({
            "ttlMS": 1_234,
            "catalog": catalog,
            "signature": {
                "algorithm": "ed25519",
                "key_id": "test",
                "signature": base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
            },
        });

        let decoded =
            decode_and_validate_document(&document.to_string(), false).expect("signed catalog");

        assert_eq!(decoded.ttl_ms, 1_234);
        assert!(decoded
            .artifact
            .models
            .iter()
            .any(|model| model.id == "refreshco/new-model"));
    }

    #[test]
    fn generated_catalog_derives_quality_tags_from_routes() {
        let catalog = artifact();
        let frontier = catalog
            .models
            .iter()
            .find(|model| model.aliases.iter().any(|alias| alias == "frontier"))
            .expect("frontier alias target is exported");
        assert!(frontier.quality_tags.iter().any(|tag| tag == "frontier"));

        let local = catalog
            .models
            .iter()
            .find(|model| model.aliases.iter().any(|alias| alias == "local-gemma4"))
            .expect("local alias target is exported");
        assert!(local.quality_tags.iter().any(|tag| tag == "local"));
    }

    #[test]
    fn validation_rejects_missing_required_metadata() {
        let mut catalog = artifact();
        catalog.providers[0].display_name.clear();
        let report = validate_artifact(&catalog);
        assert!(
            report
                .errors
                .iter()
                .any(|message| message.contains("display_name cannot be empty")),
            "expected provider metadata validation error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn validation_rejects_duplicate_and_dangling_aliases() {
        let mut duplicated = artifact();
        duplicated.aliases.push(duplicated.aliases[0].clone());
        let duplicate_report = validate_artifact(&duplicated);
        assert!(
            duplicate_report
                .errors
                .iter()
                .any(|message| message.contains("duplicate alias name")),
            "expected duplicate alias validation error, got {:?}",
            duplicate_report.errors
        );

        let mut dangling = artifact();
        dangling.aliases[0].model_id = "missing-model".to_string();
        let dangling_report = validate_artifact(&dangling);
        assert!(
            dangling_report
                .errors
                .iter()
                .any(|message| message.contains("without a catalog row")),
            "expected dangling alias validation error, got {:?}",
            dangling_report.errors
        );
    }

    #[test]
    fn overlay_merge_surfaces_private_model() {
        let _guard = install_overlay(
            r#"
[providers.private]
display_name = "Private"
base_url = "http://127.0.0.1:9000"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"

[aliases]
private-fast = { id = "private/fast", provider = "private" }

[models."private/fast"]
name = "Private Fast"
provider = "private"
context_window = 8192
quality_tags = ["experiment"]
"#,
        );
        let catalog = artifact();
        assert!(catalog.providers.iter().any(|p| p.id == "private"));
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == "private/fast")
            .expect("private model is exported");
        assert_eq!(model.aliases, vec!["private-fast"]);
        assert_eq!(model.quality_tags, vec!["experiment"]);
    }

    #[test]
    fn cataloged_models_default_to_serverless_availability() {
        llm_config::clear_user_overrides();
        let catalog = artifact();
        let qwen_dedicated = catalog
            .models
            .iter()
            .find(|model| model.id == "Qwen/Qwen3-Coder-Next-FP8")
            .expect("Together dedicated route is exported");
        assert_eq!(
            qwen_dedicated.availability,
            ModelAvailabilityStatus::Dedicated
        );

        let bundled_serverless = catalog
            .models
            .iter()
            .find(|model| model.id == "qwen/qwen3-coder")
            .expect("OpenRouter Qwen3 Coder is exported");
        assert_eq!(
            bundled_serverless.availability,
            ModelAvailabilityStatus::Serverless
        );
    }

    #[test]
    fn tier_alias_targeting_dedicated_model_emits_warning() {
        let _guard = install_overlay(
            r#"
[providers.together_test]
display_name = "Together (test)"
base_url = "https://api.together.xyz/v1"
auth_style = "bearer"
auth_env = "TOGETHER_AI_API_KEY"
chat_endpoint = "/chat/completions"

[aliases.frontier]
id = "Qwen/Test-Dedicated-Only"
provider = "together_test"

[models."Qwen/Test-Dedicated-Only"]
name = "Qwen Dedicated Only"
provider = "together_test"
context_window = 8192
availability = "dedicated"
"#,
        );
        let report = validate_current();
        assert!(
            report.warnings.iter().any(|message| {
                message.contains("tier alias frontier") && message.contains("dedicated-only model")
            }),
            "expected dedicated-alias warning, got {:?}",
            report.warnings
        );
    }

    #[test]
    fn overlay_parses_availability_strings() {
        let _guard = install_overlay(
            r#"
[providers.experiment_co]
display_name = "Experiment Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "EXPERIMENT_API_KEY"
chat_endpoint = "/chat/completions"

[models."exp/discovered"]
name = "Discovered Route"
provider = "experiment_co"
context_window = 4096
availability = "unknown"
"#,
        );
        let catalog = artifact();
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == "exp/discovered")
            .expect("overlay model is exported");
        assert_eq!(model.availability, ModelAvailabilityStatus::Unknown);
    }

    #[test]
    fn catalog_exports_family_and_lineage_for_hosted_wrappers() {
        let catalog = artifact();
        let hosted_claude = catalog
            .models
            .iter()
            .find(|model| model.id == "anthropic/claude-sonnet-4-6")
            .expect("OpenRouter Claude wrapper is exported");
        assert_eq!(hosted_claude.provider, "openrouter");
        assert_eq!(hosted_claude.family, "anthropic-claude");
        assert_eq!(hosted_claude.lineage, "claude-sonnet-opus");

        let direct_gemini = catalog
            .models
            .iter()
            .find(|model| model.id == "gemini-2.5-flash")
            .expect("Gemini Flash is exported");
        assert_eq!(direct_gemini.family, "google-gemini");
        assert_eq!(direct_gemini.lineage, "gemini-flash");
    }

    #[test]
    fn validation_rejects_malformed_family_metadata() {
        let mut catalog = artifact();
        catalog.models[0].family = "Not Normalized".to_string();
        catalog.models[0].lineage.clear();
        let report = validate_artifact(&catalog);
        assert!(
            report
                .errors
                .iter()
                .any(|message| message.contains("family")),
            "expected family validation error, got {:?}",
            report.errors
        );
        assert!(
            report
                .errors
                .iter()
                .any(|message| message.contains("lineage")),
            "expected lineage validation error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn deprecated_models_require_notes() {
        let _guard = install_overlay(
            r#"
[models."old-model"]
name = "Old Model"
provider = "openai"
context_window = 4096
deprecated = true
"#,
        );
        let report = validate_current();
        assert!(
            report
                .errors
                .iter()
                .any(|message| message.contains("deprecated model old-model")),
            "expected deprecation validation error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn generated_schema_accepts_generated_artifact_shape() {
        let schema = schema_value();
        assert_eq!(schema["$id"], PROVIDER_CATALOG_SCHEMA_ID);
        assert_eq!(
            schema["$defs"]["tool_support"]["properties"]["empirical_parity"]["$ref"],
            "#/$defs/tool_empirical_parity"
        );
        assert!(schema["$defs"]["model"]["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|field| field == "family")));
        assert!(schema["$defs"]["model"]["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|field| field == "lineage")));
        let artifact_value = serde_json::to_value(artifact()).expect("artifact serializes");
        assert_eq!(
            artifact_value["schema_version"],
            PROVIDER_CATALOG_SCHEMA_VERSION
        );
        assert!(artifact_value["providers"]
            .as_array()
            .is_some_and(|v| !v.is_empty()));
        assert!(artifact_value["models"]
            .as_array()
            .is_some_and(|v| !v.is_empty()));
        assert!(artifact_value["models"][0]["family"].is_string());
        assert!(artifact_value["models"][0]["lineage"].is_string());
    }

    #[test]
    fn downstream_bindings_include_empirical_tool_parity_shape() {
        let typescript = typescript_binding().expect("typescript binding renders");
        assert!(typescript.contains("empirical_parity?: HarnToolEmpiricalParity"));
        assert!(typescript.contains("export interface HarnToolEmpiricalParity"));

        let swift = swift_binding().expect("swift binding renders");
        assert!(swift.contains("public let empiricalParity: HarnToolEmpiricalParity?"));
        assert!(swift.contains("public struct HarnToolEmpiricalParity"));
    }

    #[test]
    fn fast_mode_and_supersession_surface_in_contract() {
        let schema = schema_value();
        assert_eq!(
            schema["$defs"]["model"]["properties"]["fast_mode"]["$ref"],
            "#/$defs/fast_mode"
        );
        assert_eq!(
            schema["$defs"]["fast_mode"]["properties"]["pricing"]["$ref"],
            "#/$defs/pricing"
        );
        assert!(schema["$defs"]["deprecation"]["properties"]["superseded_by"].is_object());

        let typescript = typescript_binding().expect("typescript binding renders");
        assert!(typescript.contains("export interface HarnModelFastMode"));
        assert!(typescript.contains("fast_mode?: HarnModelFastMode"));
        assert!(typescript.contains("superseded_by?: string"));
        assert!(typescript.contains("family: string"));
        assert!(typescript.contains("lineage: string"));

        let swift = swift_binding().expect("swift binding renders");
        assert!(swift.contains("public struct HarnModelFastMode"));
        assert!(swift.contains("public let fastMode: HarnModelFastMode?"));
        assert!(swift.contains("case supersededBy = \"superseded_by\""));
        assert!(swift.contains("public let family: String"));
        assert!(swift.contains("public let lineage: String"));
    }

    #[test]
    fn dangling_superseded_by_and_unknown_fast_status_warn() {
        let _guard = install_overlay(
            r#"
[providers.warn_co]
display_name = "Warn Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "WARN_API_KEY"
chat_endpoint = "/chat/completions"

[models."warn/old"]
name = "Warn Old"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Retiring soon."
superseded_by = "warn/does-not-exist"
fast_mode = { param = "speed", value = "fast", status = "turbo", pricing = { input_per_mtok = 1.0, output_per_mtok = 2.0 } }
"#,
        );
        let report = validate_current();
        assert!(
            report
                .warnings
                .iter()
                .any(|message| message.contains("superseded_by warn/does-not-exist")),
            "expected dangling superseded_by warning, got {:?}",
            report.warnings
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|message| message.contains("fast_mode.status") && message.contains("turbo")),
            "expected fast_mode.status warning, got {:?}",
            report.warnings
        );
    }
}
