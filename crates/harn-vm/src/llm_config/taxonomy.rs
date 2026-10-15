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
    if model_id.starts_with("local:")
        || model_id.starts_with("ollama:")
        || model_id.starts_with("ollama/")
    {
        return crate::llm::provider::ProviderInference::builtin("ollama");
    }
    if model_id.starts_with("huggingface:") || model_id.starts_with("hf:") {
        return crate::llm::provider::ProviderInference::builtin("huggingface");
    }
    if let Some(provider) = configured_provider_selector(config, model_id) {
        return crate::llm::provider::ProviderInference::builtin(provider);
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

pub(super) use harn_provider_catalog::configured_provider_selector;

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

pub(crate) use harn_provider_catalog::taxonomy::{
    derive_model_family, derive_model_lineage, model_family_with_config, model_lineage_with_config,
    model_tier_with_config, normalized_catalog_token,
};

/// Return the normalized model-family token used for cross-family review.
pub fn model_family(provider: &str, model_id: &str) -> String {
    let config = effective_config();
    model_family_with_config(&config, provider, model_id)
}

pub(crate) fn model_family_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(family) = config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.family.as_deref()))
    {
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

pub(crate) fn model_lineage_with_inference_source(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
    source: crate::llm::provider::ProviderInferenceSource,
) -> String {
    if let Some(lineage) = config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.lineage.as_deref()))
    {
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
