//! Model taxonomy and provider inference: map a model id to its provider,
//! tier, family, and lineage, plus default/auto provider selection.
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

// Model-pattern matching for forms like "claude-*", "qwen/*", "ollama:*".
// Shared workspace semantics live in `harn-glob`.
use harn_glob::match_name as glob_match;

/// Infer provider from a model ID using inference rules.
pub fn infer_provider(model_id: &str) -> String {
    infer_provider_detail(model_id).provider
}

/// Infer provider from a model ID and retain whether the configured default was used.
pub(crate) fn infer_provider_detail(model_id: &str) -> crate::llm::provider::ProviderInference {
    let config = effective_config();
    infer_provider_with_config(&config, model_id)
}

pub(crate) fn infer_provider_with_config(
    config: &ProvidersConfig,
    model_id: &str,
) -> crate::llm::provider::ProviderInference {
    if model_id.starts_with("local:") || model_id.starts_with("ollama:") {
        return crate::llm::provider::ProviderInference::builtin("ollama");
    }
    if model_id.starts_with("huggingface:") || model_id.starts_with("hf:") {
        return crate::llm::provider::ProviderInference::builtin("huggingface");
    }
    // Exact catalog rows are the most authoritative declaration of where
    // a model is hosted: any pattern-based inference rule is necessarily
    // less specific than `[models."<id>"].provider = "<name>"`. Catalogs
    // include user overlays, so users can still re-home a model by
    // setting a catalog entry in their own providers.toml.
    let normalized_id = normalize_model_id(model_id);
    if let Some(model) = config
        .models
        .get(model_id)
        .or_else(|| config.models.get(&normalized_id))
    {
        return crate::llm::provider::ProviderInference::builtin(model.provider.clone());
    }
    for rule in &config.inference_rules {
        if let Some(exact) = &rule.exact {
            if model_id == exact {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
        if let Some(pattern) = &rule.pattern {
            if glob_match(pattern, model_id) {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
        if let Some(substr) = &rule.contains {
            if model_id.contains(substr.as_str()) {
                return crate::llm::provider::ProviderInference::builtin(rule.provider.clone());
            }
        }
    }
    crate::llm::provider::infer_provider_from_model_id(
        model_id,
        &default_provider_with_config(config),
    )
}

pub fn default_provider() -> String {
    let config = effective_config();
    default_provider_with_config(&config)
}

fn default_provider_with_config(config: &ProvidersConfig) -> String {
    crate::stdlib::process::session_env_value("HARN_DEFAULT_PROVIDER")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"))
        .or_else(|| {
            config
                .default_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| auto_select_provider(config))
}

/// Provider assumed when nothing is configured and no credentials are found.
/// Anthropic is Harn's documented default; [`auto_select_provider`] only falls
/// back to it after probing for a credentialed or local provider, and warns
/// once so adopters without Anthropic credentials get a clear nudge instead of
/// a raw auth failure.
pub(crate) const FALLBACK_PROVIDER: &str = "anthropic";

static AUTO_PROVIDER_WARNED: AtomicBool = AtomicBool::new(false);

fn provider_has_credentials(name: &str, def: &ProviderDef) -> bool {
    matches!(
        crate::llm::provider_auth_status_with_definition(name, def).credential_status,
        crate::llm::ProviderCredentialStatus::Ok | crate::llm::ProviderCredentialStatus::Deferred
    )
}

/// True when the provider can serve without cloud credentials — a managed
/// local runtime (`harn local`) or an auth-free endpoint such as Ollama.
pub(crate) fn provider_is_local(def: &ProviderDef) -> bool {
    def.local_runtime.is_some() || matches!(def.auth_env, AuthEnv::None)
}

/// Emit a provider auto-selection notice at most once per process.
fn warn_auto_provider_once(message: &str) {
    if !AUTO_PROVIDER_WARNED.swap(true, Ordering::Relaxed) {
        crate::events::log_warn("llm_config", message);
    }
}

/// Choose a provider when neither `HARN_DEFAULT_PROVIDER` nor
/// `config.default_provider` is set. Prefers a credentialed cloud provider,
/// then a locally-available one, and only then falls back to the documented
/// default. Detection is portable: it reads provider `auth_env` variables and
/// `local_runtime` metadata from the catalog — never hardcoded paths or ports.
pub(crate) fn auto_select_provider(config: &ProvidersConfig) -> String {
    // Well-known providers first for a stable, predictable choice; then any
    // other configured provider (BTreeMap iteration is sorted/deterministic).
    const PREFERRED: &[&str] = &[
        "anthropic",
        "openai",
        "google",
        "azure-openai",
        "groq",
        "mistral",
        "deepseek",
        "xai",
        "openrouter",
    ];
    for name in PREFERRED {
        if config
            .providers
            .get(*name)
            .is_some_and(|definition| provider_has_credentials(name, definition))
        {
            if *name != FALLBACK_PROVIDER {
                warn_auto_provider_once(&format!(
                    "no default provider configured; using '{name}' (its API key is set). \
                     Set HARN_DEFAULT_PROVIDER or `default_provider` to silence this."
                ));
            }
            return (*name).to_string();
        }
    }
    for (name, def) in &config.providers {
        if provider_has_credentials(name, def) {
            warn_auto_provider_once(&format!(
                "no default provider configured; using '{name}' (its API key is set). \
                 Set HARN_DEFAULT_PROVIDER or `default_provider` to silence this."
            ));
            return name.clone();
        }
    }
    // No cloud credentials: prefer something that runs locally with no key.
    for (name, def) in &config.providers {
        if provider_is_local(def) {
            warn_auto_provider_once(&format!(
                "no provider API keys found; using local provider '{name}'. \
                 Set an API key + HARN_DEFAULT_PROVIDER to use a cloud provider."
            ));
            return name.clone();
        }
    }
    // Nothing detected. Fall back to the documented default and say how to fix.
    warn_auto_provider_once(&format!(
        "no LLM provider configured and no API keys detected; defaulting to \
         '{FALLBACK_PROVIDER}'. Set ANTHROPIC_API_KEY (or another provider's key plus \
         HARN_DEFAULT_PROVIDER), or run a local model with `harn local launch`."
    ));
    FALLBACK_PROVIDER.to_string()
}

/// Get model tier ("small", "mid", "frontier").
pub fn model_tier(model_id: &str) -> String {
    let config = effective_config();
    model_tier_with_config(&config, model_id)
}

pub(crate) fn model_tier_with_config(config: &ProvidersConfig, model_id: &str) -> String {
    // Per-model self-declared tier wins. This is the only path.
    if let Some(model) = config.models.get(model_id) {
        if let Some(tier) = model.tier.as_deref() {
            let trimmed = tier.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    // Legacy pattern-rules: still consulted while we finish migrating the
    // long tail of models to per-row `tier = "..."`. Newly added rows
    // should set `tier` directly; the rule table is a fallback only.
    for rule in &config.tier_rules {
        if let Some(exact) = &rule.exact {
            if model_id == exact {
                return rule.tier.clone();
            }
        }
        if let Some(pattern) = &rule.pattern {
            if glob_match(pattern, model_id) {
                return rule.tier.clone();
            }
        }
        if let Some(substr) = &rule.contains {
            if model_id.contains(substr.as_str()) {
                return rule.tier.clone();
            }
        }
    }
    config.tier_defaults.default.clone()
}

/// Return the normalized model-family token used for cross-family review.
pub fn model_family(provider: &str, model_id: &str) -> String {
    let config = effective_config();
    model_family_with_config(&config, provider, model_id)
}

pub(crate) fn model_family_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    catalog_family_token(config, model_id)
        .unwrap_or_else(|| derive_model_family(provider, model_id))
}

pub(crate) fn model_family_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(family) = catalog_family_token(config, model_id) {
        return family;
    }
    let id_family = derive_model_family("", model_id);
    if id_family != "unknown" {
        return id_family;
    }
    if matches!(
        source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    ) {
        return "unknown".to_string();
    }
    derive_model_family(provider, model_id)
}

/// Return the narrower lineage token used for model-aware option packs.
pub fn model_lineage(provider: &str, model_id: &str) -> String {
    let config = effective_config();
    model_lineage_with_config(&config, provider, model_id)
}

pub(crate) fn model_lineage_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    catalog_lineage_token(config, model_id)
        .unwrap_or_else(|| derive_model_lineage(provider, model_id))
}

pub(crate) fn model_lineage_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(lineage) = catalog_lineage_token(config, model_id) {
        return lineage;
    }
    let id_lineage = derive_model_lineage("", model_id);
    if id_lineage != "unknown" {
        return id_lineage;
    }
    if matches!(
        source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    ) {
        return "unknown".to_string();
    }
    derive_model_lineage(provider, model_id)
}

fn catalog_family_token(config: &ProvidersConfig, model_id: &str) -> Option<String> {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.family.as_deref()))
}

fn catalog_lineage_token(config: &ProvidersConfig, model_id: &str) -> Option<String> {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.lineage.as_deref()))
}

pub(crate) fn normalized_catalog_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase().replace('_', "-"))
}

fn derive_model_family(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["claude", "anthropic.claude"]) {
        return "anthropic-claude".to_string();
    }
    if contains_any(&id, &["gemini", "google/gemini"]) {
        return "google-gemini".to_string();
    }
    if contains_any(&id, &["deepseek"]) {
        return "deepseek".to_string();
    }
    if contains_any(&id, &["qwen"]) {
        return "qwen".to_string();
    }
    if contains_any(&id, &["kimi", "moonshot"]) {
        return "kimi".to_string();
    }
    if contains_any(&id, &["glm", "z-ai/glm", "zhipu"]) {
        return "glm".to_string();
    }
    if contains_any(&id, &["mistral", "mixtral", "devstral"]) {
        return "mistral".to_string();
    }
    if contains_any(&id, &["minimax"]) {
        return "minimax".to_string();
    }
    if contains_any(&id, &["llama"]) {
        return "llama".to_string();
    }
    if contains_any(&id, &["gemma"]) {
        return "gemma".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-oss", "openai/gpt", "gpt-"]) {
        return "openai-gpt".to_string();
    }
    match provider {
        "anthropic" | "bedrock" | "vertex-anthropic" => "anthropic-claude".to_string(),
        "openai" | "azure" | "azure_openai" => "openai-gpt".to_string(),
        "gemini" | "vertex" | "google" => "google-gemini".to_string(),
        "deepseek" => "deepseek".to_string(),
        "zai" => "glm".to_string(),
        "minimax" => "minimax".to_string(),
        other if !other.is_empty() => normalize_identifier_token(other),
        _ => "unknown".to_string(),
    }
}

fn derive_model_lineage(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["haiku"]) {
        return "claude-haiku".to_string();
    }
    if contains_any(&id, &["opus-4-7", "opus-4-8", "opus-mythos"]) {
        return "claude-opus-adaptive".to_string();
    }
    if contains_any(&id, &["claude"]) {
        return "claude-sonnet-opus".to_string();
    }
    if contains_any(&id, &["gpt-5"]) {
        return "openai-gpt5".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-", "gpt_"]) {
        return "openai-legacy".to_string();
    }
    if contains_any(&id, &["gemini"]) {
        if contains_any(&id, &["flash"]) {
            return "gemini-flash".to_string();
        }
        return "gemini-pro".to_string();
    }
    if contains_any(&id, &["qwen3", "qwen/qwen3"]) {
        return "qwen3".to_string();
    }
    if contains_any(&id, &["gemma4", "gemma-4"]) {
        return "gemma4".to_string();
    }
    let family = derive_model_family(provider, model_id);
    if family == "unknown" {
        "unknown".to_string()
    } else {
        family
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn starts_with_any(haystack: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| haystack.starts_with(prefix))
}

fn is_openai_reasoning_model(id: &str) -> bool {
    starts_with_any(id, &["o1", "o3", "o4"])
        || contains_any(
            id,
            &[
                "/o1", "/o3", "/o4", ":o1", ":o3", ":o4", ".o1", ".o3", ".o4",
            ],
        )
}

fn normalize_identifier_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
