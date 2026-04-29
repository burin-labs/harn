use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use crate::events::{emit_log, EventLevel};
use crate::value::{VmError, VmValue};

/// Cache of provider name -> whether a usable API key is available.
/// Cached for process lifetime since env vars don't change mid-run.
static PROVIDER_KEY_CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
static MODEL_TIER_WARNING_CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Check whether `provider` has a usable API key (or needs none). Cached.
pub(crate) fn provider_key_available(provider: &str) -> bool {
    let cache = PROVIDER_KEY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(&available) = map.get(provider) {
        return available;
    }
    let available = resolve_api_key(provider).is_ok();
    map.insert(provider.to_string(), available);
    available
}

/// Clear the provider key cache (for tests that manipulate env vars).
#[cfg(test)]
pub(crate) fn reset_provider_key_cache() {
    if let Some(cache) = PROVIDER_KEY_CACHE.get() {
        cache.lock().unwrap().clear();
    }
}

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

    if provider_key_available(&selected_provider) {
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
        if preferred_provider == Some(provider.as_str()) && provider_key_available(provider) {
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
        if !provider_key_available(&provider) {
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
        if provider_key_available(provider) {
            return Some((model.clone(), provider.clone()));
        }
    }

    requested
}

pub(crate) fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
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
    }
    if let Ok(p) = std::env::var("HARN_LLM_PROVIDER") {
        return p;
    }
    // First-class local OpenAI-compatible server support.
    if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
        && (options.as_ref().and_then(|o| o.get("model")).is_some()
            || std::env::var("HARN_LLM_MODEL").is_ok()
            || std::env::var("LOCAL_LLM_MODEL").is_ok())
    {
        return "local".to_string();
    }
    if let Some(m) = options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
    {
        return llm_config::infer_provider(&m);
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
        return llm_config::infer_provider(&m);
    }
    // Default to anthropic, but fall back to keyless providers when its
    // key is missing - avoids noisy errors when a sub-pipeline (e.g.
    // enrichment) didn't inherit the provider env.
    let default = "anthropic";
    if provider_key_available(default) {
        return default.to_string();
    }
    for fallback in ["ollama", "local"] {
        if provider_key_available(fallback) {
            return fallback.to_string();
        }
    }
    // Let resolve_api_key surface its descriptive error.
    default.to_string()
}

pub(crate) fn vm_resolve_model(
    options: &Option<BTreeMap<String, VmValue>>,
    provider: &str,
) -> String {
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
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .or_else(|_| std::env::var("HARN_LLM_MODEL"))
            .unwrap_or_else(|_| "gpt-4o".to_string()),
        "mlx" => std::env::var("MLX_MODEL_ID")
            .unwrap_or_else(|_| "unsloth/Qwen3.6-27B-UD-MLX-4bit".to_string()),
        "openai" => "gpt-4o".to_string(),
        "ollama" => "llama3.2".to_string(),
        "openrouter" => "anthropic/claude-sonnet-4.6".to_string(),
        _ => "claude-sonnet-4-20250514".to_string(),
    }
}

pub fn resolve_api_key(provider: &str) -> Result<String, VmError> {
    use crate::llm_config;

    if provider == "mock"
        || crate::llm::mock::cli_llm_mock_replay_active()
        || crate::llm::mock::builtin_llm_mock_active()
    {
        return Ok(String::new());
    }

    // These providers use multi-step platform auth that is resolved inside
    // their provider shims: Bedrock walks the AWS credential chain and Vertex
    // accepts bearer tokens or service-account JSON. Returning an empty string
    // here keeps generic option extraction from rejecting valid profile /
    // instance-role / ADC setups before the provider can inspect them.
    if matches!(provider, "bedrock" | "vertex") {
        return Ok(String::new());
    }

    // Explain provenance (env vs llm.toml vs default) and how to opt into mock.
    let selection_hint = {
        let config_path = llm_config::loaded_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<built-in defaults>".to_string());
        format!(
            " (provider '{provider}' selected via LLM_PROVIDER / llm.toml @ {config_path}; \
             set HARN_LLM_PROVIDER=mock or LLM_PROVIDER=mock for offline use)"
        )
    };

    if let Some(pdef) = llm_config::provider_config(provider) {
        if pdef.auth_style == "none" {
            return Ok(String::new());
        }
        match &pdef.auth_env {
            llm_config::AuthEnv::Single(env) => {
                return std::env::var(env).map_err(|_| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Missing API key: set {env} environment variable{selection_hint}"
                    ))))
                });
            }
            llm_config::AuthEnv::Multiple(envs) => {
                for env in envs {
                    if let Ok(val) = std::env::var(env) {
                        if !val.is_empty() {
                            return Ok(val);
                        }
                    }
                }
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Missing API key: set one of {} environment variables{selection_hint}",
                    envs.join(", ")
                )))));
            }
            llm_config::AuthEnv::None => return Ok(String::new()),
        }
    }
    std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "Missing API key: set ANTHROPIC_API_KEY environment variable{selection_hint}"
        ))))
    })
}

pub(crate) struct ResolvedProvider {
    pub pdef: Option<crate::llm_config::ProviderDef>,
    pub is_anthropic_style: bool,
    pub base_url: String,
    pub endpoint: String,
}

impl ResolvedProvider {
    pub fn resolve(provider: &str) -> ResolvedProvider {
        let pdef = crate::llm_config::provider_config(provider);
        let is_anthropic_style = pdef
            .as_ref()
            .map(|p| p.chat_endpoint.contains("/messages"))
            .unwrap_or(provider == "anthropic");
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
            is_anthropic_style,
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
