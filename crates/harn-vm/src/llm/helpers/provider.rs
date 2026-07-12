use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::events::{emit_log, EventLevel};

static MODEL_TIER_WARNING_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static PROVIDER_INFERENCE_WARNING_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn push_unique(items: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() && !items.iter().any(|existing| existing == &value) {
        items.push(value);
    }
}

fn warn_model_tier_fallback(target: &str, requested_provider: Option<&str>, chosen: (&str, &str)) {
    let key = format!(
        "{target}|{}|{}|{}",
        requested_provider.unwrap_or(""),
        chosen.0,
        chosen.1
    );
    let cache = MODEL_TIER_WARNING_CACHE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = cache.lock().unwrap();
    if !guard.insert(key) {
        return;
    }
    drop(guard);

    emit_log(
        EventLevel::Warn,
        "llm",
        &format!(
            "model_tier '{target}' could not use provider '{}' in the current environment; \
             falling back to reachable provider '{}' with model '{}'",
            requested_provider.unwrap_or("the default tier mapping"),
            chosen.1,
            chosen.0
        ),
        BTreeMap::new(),
    );
}

fn warn_provider_default_fallback(model_id: &str, provider: &str) {
    let key = format!("{model_id}|{provider}");
    let cache = PROVIDER_INFERENCE_WARNING_CACHE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = cache.lock().unwrap();
    if !guard.insert(key) {
        return;
    }
    drop(guard);

    crate::events::log_warn_meta(
        "llm.provider",
        &format!(
            "could not infer provider from model id '{model_id}'; falling back to default provider '{provider}'"
        ),
        BTreeMap::from([
            (
                "model".to_string(),
                serde_json::Value::String(model_id.to_string()),
            ),
            (
                "provider".to_string(),
                serde_json::Value::String(provider.to_string()),
            ),
            (
                "reason".to_string(),
                serde_json::Value::String("default_provider_fallback".to_string()),
            ),
        ]),
    );
}

fn infer_provider_from_model_selector(raw_model: &str, warn_on_default: bool) -> String {
    use crate::llm::provider::ProviderInferenceSource;
    use crate::llm_config;

    let (_resolved_model, resolved_provider) = llm_config::resolve_model(raw_model);
    if let Some(provider) = resolved_provider {
        return provider;
    }

    let inference = llm_config::infer_provider_detail(raw_model);
    if warn_on_default && inference.source == ProviderInferenceSource::DefaultFallback {
        warn_provider_default_fallback(raw_model, &inference.provider);
    }
    inference.provider
}

/// Read the pinned model selector for the session currently on the
/// VM's session stack, if any. Used by `vm_resolve_provider` /
/// `vm_resolve_model` to honour ACP `session/set_config_option`
/// (configId="model") swaps without each builtin needing to thread the
/// session id manually.
fn current_session_pinned_model() -> Option<String> {
    let id = crate::agent_sessions::current_session_id()?;
    crate::agent_sessions::pinned_model(&id)
}

fn env_selected_model_for_tier() -> Option<(String, String)> {
    use crate::llm_config;

    let selected_model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .or_else(|| std::env::var("LOCAL_LLM_MODEL").ok())?;

    let selected_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|provider| !provider.is_empty())
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                Some("local".to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| llm_config::infer_provider(&selected_model));

    if crate::llm::provider_auth_status(&selected_provider).available {
        Some((selected_model, selected_provider))
    } else {
        None
    }
}

fn preferred_provider_order(preferred_provider: Option<&str>) -> Vec<String> {
    use crate::llm_config;

    let mut providers = Vec::new();
    if let Some(provider) = preferred_provider {
        push_unique(&mut providers, provider.to_string());
    }
    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        push_unique(&mut providers, provider);
    }
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
        push_unique(&mut providers, "local");
    }
    if let Ok(model) = std::env::var("HARN_LLM_MODEL") {
        push_unique(&mut providers, llm_config::infer_provider(&model));
    }
    if let Ok(model) = std::env::var("LOCAL_LLM_MODEL") {
        push_unique(&mut providers, llm_config::infer_provider(&model));
    }
    for provider in [
        "local",
        "ollama",
        "openrouter",
        "together",
        "huggingface",
        "openai",
        "anthropic",
    ] {
        push_unique(&mut providers, provider);
    }
    providers
}

fn resolve_available_tier_model(
    target: &str,
    preferred_provider: Option<&str>,
) -> Option<(String, String)> {
    use crate::llm_config;

    let requested = llm_config::resolve_tier_model(target, preferred_provider);
    if let Some((model, provider)) = requested.as_ref() {
        if preferred_provider == Some(provider.as_str())
            && crate::llm::provider_auth_status(provider).available
        {
            return Some((model.clone(), provider.clone()));
        }
    }

    if let Some((model, provider)) = env_selected_model_for_tier() {
        if requested
            .as_ref()
            .map(|(_, requested_provider)| requested_provider != &provider)
            .unwrap_or(true)
        {
            warn_model_tier_fallback(
                target,
                requested.as_ref().map(|(_, provider)| provider.as_str()),
                (&model, &provider),
            );
        }
        return Some((model, provider));
    }

    let candidates = llm_config::tier_candidates(target);
    for provider in preferred_provider_order(preferred_provider) {
        if !crate::llm::provider_auth_status(&provider).available {
            continue;
        }
        if let Some((model, candidate_provider)) = candidates
            .iter()
            .find(|(_, candidate_provider)| candidate_provider == &provider)
        {
            if requested
                .as_ref()
                .map(|(_, requested_provider)| requested_provider != candidate_provider)
                .unwrap_or(true)
            {
                warn_model_tier_fallback(
                    target,
                    requested.as_ref().map(|(_, provider)| provider.as_str()),
                    (model, candidate_provider),
                );
            }
            return Some((model.clone(), candidate_provider.clone()));
        }
    }

    if let Some((model, provider)) = requested.as_ref() {
        if crate::llm::provider_auth_status(provider).available {
            return Some((model.clone(), provider.clone()));
        }
    }

    requested
}

pub(crate) fn vm_resolve_provider(options: &Option<crate::value::DictMap>) -> String {
    use crate::llm_config;

    // Explicit option wins, except "auto" which means "run the normal
    // inference chain". Treating "auto" as a literal provider name would
    // make resolve_api_key default to anthropic and fail whenever
    // ANTHROPIC_API_KEY is absent, breaking any sub-call that couldn't
    // inspect the env itself.
    if let Some(p) = options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
    {
        if !p.eq_ignore_ascii_case("auto") {
            return p;
        }
        if let Some(m) = options
            .as_ref()
            .and_then(|o| o.get("model"))
            .map(|v| v.display())
        {
            return infer_provider_from_model_selector(&m, true);
        }
    }
    if let Some(pinned) = current_session_pinned_model() {
        return infer_provider_from_model_selector(&pinned, true);
    }
    if let Ok(p) = std::env::var("HARN_LLM_PROVIDER") {
        return p;
    }
    // When an explicit `model:` option is set, prefer a catalog-known
    // provider over the LOCAL_LLM_BASE_URL fast-path. Without this,
    // a user with `LOCAL_LLM_BASE_URL` pointing at Ollama silently
    // routes every catalog-known model
    // (e.g. `anthropic/claude-sonnet-4-6` → openrouter,
    // `qwen-3-coder-480b` → cerebras) to their local server and
    // gets a `model_unavailable` 404 instead of the real provider.
    let explicit_model = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display());
    if let Some(ref m) = explicit_model {
        use crate::llm::provider::ProviderInferenceSource;
        // 1. Direct `[aliases]` match wins immediately.
        let (_resolved, alias_provider) = llm_config::resolve_model(m);
        if let Some(provider) = alias_provider {
            return provider;
        }
        // 2. `[models]` table + `[[inference_rules]]` matches are also
        //    authoritative. Only a DefaultFallback (nothing matched)
        //    falls through to the local fast-path below.
        let inference = llm_config::infer_provider_detail(m);
        if inference.source != ProviderInferenceSource::DefaultFallback {
            return inference.provider;
        }
    }
    // First-class local OpenAI-compatible server support: only kicks in
    // when the model isn't catalog-known, so unknown ids still route to
    // the local server.
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
        && (explicit_model.is_some()
            || std::env::var("HARN_LLM_MODEL").is_ok()
            || std::env::var("LOCAL_LLM_MODEL").is_ok())
    {
        return "local".to_string();
    }
    if let Some(m) = explicit_model {
        return infer_provider_from_model_selector(&m, true);
    }
    if let Some(tier) = options
        .as_ref()
        .and_then(|o| o.get("model_tier"))
        .map(|v| v.display())
    {
        if let Some((_, provider)) = resolve_available_tier_model(&tier, None) {
            return provider;
        }
    }
    if let Ok(m) = std::env::var("HARN_LLM_MODEL") {
        return infer_provider_from_model_selector(&m, true);
    }
    // Default to anthropic, but fall back to keyless providers when its
    // key is missing - avoids noisy errors when a sub-pipeline (e.g.
    // enrichment) didn't inherit the provider env.
    let default = llm_config::default_provider();
    if crate::llm::provider_auth_status(&default).available {
        return default;
    }
    for fallback in ["ollama", "local"] {
        if crate::llm::provider_auth_status(fallback).available {
            return fallback.to_string();
        }
    }
    // Let resolve_api_key surface its descriptive error.
    default
}

pub(crate) fn vm_resolve_model(options: &Option<crate::value::DictMap>, provider: &str) -> String {
    use crate::llm_config;

    if let Some(raw) = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
    {
        let (resolved, _) = llm_config::resolve_model(&raw);
        return resolved;
    }
    if let Some(tier) = options
        .as_ref()
        .and_then(|o| o.get("model_tier"))
        .map(|v| v.display())
    {
        if let Some((resolved, _)) = resolve_available_tier_model(&tier, Some(provider)) {
            return resolved;
        }
    }
    if let Some(pinned) = current_session_pinned_model() {
        let (resolved, resolved_provider) = llm_config::resolve_model(&pinned);
        let inferred_provider =
            resolved_provider.unwrap_or_else(|| infer_provider_from_model_selector(&pinned, false));
        if inferred_provider == provider {
            return resolved;
        }
    }
    if let Ok(raw) = std::env::var("HARN_LLM_MODEL") {
        let (resolved, resolved_provider) = llm_config::resolve_model(&raw);
        let env_provider = std::env::var("HARN_LLM_PROVIDER").ok();
        if resolved_provider.as_deref() == Some(provider)
            || (resolved_provider.is_none() && env_provider.as_deref() == Some(provider))
        {
            return resolved;
        }
    }
    if provider == "local" {
        if let Ok(raw) = std::env::var("LOCAL_LLM_MODEL") {
            let (resolved, _) = llm_config::resolve_model(&raw);
            return resolved;
        }
    }
    llm_config::default_model_for_provider(provider)
}

pub(crate) struct ResolvedProvider {
    pub pdef: Option<crate::llm_config::ProviderDef>,
    pub base_url: String,
    pub endpoint: String,
}

impl ResolvedProvider {
    pub fn resolve(provider: &str) -> ResolvedProvider {
        let pdef = crate::llm_config::provider_config(provider);
        let is_anthropic_style = pdef
            .as_ref()
            .map(|p| p.chat_endpoint.contains("/messages"))
            .unwrap_or_else(|| {
                crate::llm::capabilities::lookup(provider, "")
                    .message_wire_format
                    .is_anthropic()
            });
        let (default_base, default_endpoint) = if is_anthropic_style {
            ("https://api.anthropic.com/v1", "/messages")
        } else {
            ("https://api.openai.com/v1", "/chat/completions")
        };
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| default_base.to_string());
        let endpoint = pdef
            .as_ref()
            .map(|p| p.chat_endpoint.clone())
            .unwrap_or_else(|| default_endpoint.to_string());
        ResolvedProvider {
            pdef,
            base_url,
            endpoint,
        }
    }

    pub fn url(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint)
    }

    pub fn apply_headers(
        &self,
        mut req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        req = crate::llm::api::apply_auth_headers(req, api_key, self.pdef.as_ref());
        if let Some(p) = self.pdef.as_ref() {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req
    }
}

#[cfg(test)]
mod no_credentials_tests {
    use crate::llm::no_credentials_message;

    #[test]
    fn message_includes_canonical_env_vars_and_doctor_hint() {
        let msg = no_credentials_message();
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "expected ANTHROPIC_API_KEY in: {msg}"
        );
        assert!(
            msg.contains("OPENAI_API_KEY"),
            "expected OPENAI_API_KEY in: {msg}"
        );
        assert!(msg.contains("harn doctor"));
        assert!(msg.contains("harn models recommend"));
        assert!(msg.contains("local Ollama"));
        assert!(msg.contains("harn-secret://namespace/name"));
    }
}

#[cfg(test)]
mod platform_managed_credential_resolution_tests {
    use crate::llm::resolve_api_key;

    /// `resolve_api_key` must defer to the provider shim for ANY provider
    /// declaring `credential_resolution = "platform_managed"` in
    /// `providers.toml`, with no per-provider name match in this function.
    /// This asserts the behavior for two independent providers (Bedrock and
    /// Vertex) that reach the same outcome through the shared capability
    /// field rather than a hardcoded `matches!(provider, "bedrock" |
    /// "vertex")` branch — a new platform-managed provider gets correct
    /// behavior by declaring the field in its `providers.toml` entry, not by
    /// adding a third arm here.
    #[test]
    fn bedrock_defers_to_provider_shim_with_no_env_vars_set() {
        // Bedrock declares no `auth_env` at all (its shim walks the AWS
        // credential chain), so even with zero relevant env vars set,
        // `resolve_api_key` must succeed with an empty placeholder rather
        // than reporting a missing API key.
        let result = resolve_api_key("bedrock");
        assert!(
            result.is_ok(),
            "expected bedrock to defer to its provider shim, got {result:?}"
        );
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn vertex_defers_to_provider_shim_even_though_it_declares_auth_env() {
        // Vertex DOES declare an `auth_env` list (VERTEX_AI_ACCESS_TOKEN /
        // GOOGLE_OAUTH_ACCESS_TOKEN / GOOGLE_APPLICATION_CREDENTIALS), which
        // would make the generic `AuthEnv::Multiple` path fail closed if none
        // of those three are set (e.g. a real ADC-only setup that resolves
        // credentials through gcloud's local metadata server instead). The
        // `credential_resolution = "platform_managed"` declaration must win
        // over the generic auth_env check.
        std::env::remove_var("VERTEX_AI_ACCESS_TOKEN");
        std::env::remove_var("GOOGLE_OAUTH_ACCESS_TOKEN");
        std::env::remove_var("GOOGLE_APPLICATION_CREDENTIALS");
        let result = resolve_api_key("vertex");
        assert!(
            result.is_ok(),
            "expected vertex to defer to its provider shim, got {result:?}"
        );
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn non_platform_managed_provider_still_enforces_auth_env() {
        // Sanity check the other side of the field: a provider that has NOT
        // opted into `credential_resolution = "platform_managed"` (Azure
        // OpenAI, which resolves through a plain multi-env bearer lookup)
        // still fails closed when none of its declared env vars are set.
        std::env::remove_var("AZURE_OPENAI_API_KEY");
        std::env::remove_var("AZURE_OPENAI_AD_TOKEN");
        std::env::remove_var("AZURE_OPENAI_BEARER_TOKEN");
        let result = resolve_api_key("azure_openai");
        assert!(
            result.is_err(),
            "expected azure_openai to require one of its declared env vars"
        );
    }
}

#[cfg(test)]
mod secret_reference_auth_tests {
    use std::sync::Mutex;

    use crate::value::{VmError, VmValue};

    use crate::llm::{provider_auth_status, resolve_api_key};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn error_message(error: VmError) -> String {
        match error {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn resolve_api_key_accepts_harn_secret_refs() {
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _provider_chain = ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");
        let _provider_key = ScopedEnvVar::set(
            "ANTHROPIC_API_KEY",
            "harn-secret://provider/anthropic-api-key",
        );
        let _secret = ScopedEnvVar::set("HARN_SECRET_PROVIDER_ANTHROPIC_API_KEY", "sk-from-ref");

        assert_eq!(resolve_api_key("anthropic").unwrap(), "sk-from-ref");
        assert!(provider_auth_status("anthropic").available);
    }

    #[test]
    fn missing_harn_secret_ref_fails_without_leaking_values() {
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let _provider_chain = ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");
        let _provider_key = ScopedEnvVar::set(
            "ANTHROPIC_API_KEY",
            "harn-secret://provider/anthropic-api-key",
        );
        let _secret = ScopedEnvVar::unset("HARN_SECRET_PROVIDER_ANTHROPIC_API_KEY");

        let message = error_message(resolve_api_key("anthropic").unwrap_err());
        assert!(
            message.contains("Failed to resolve API key secret reference from ANTHROPIC_API_KEY")
        );
        assert!(message.contains("provider/anthropic-api-key"));
        assert!(!message.contains("sk-from-ref"));
        assert!(!provider_auth_status("anthropic").available);
    }
}
