//! Per-model context-window discovery + auto-compaction threshold
//! resolution.
//!
//! OpenAI-compatible servers (vLLM, text-generation-inference, LocalAI,
//! llama.cpp server) expose `max_model_len` via `GET /v1/models`. Query it
//! once so auto-compaction thresholds match the real window instead of
//! assuming 80K and letting the server silently truncate older turns.

use std::collections::HashMap as StdHashMap;
use std::sync::{Mutex as StdMutex, OnceLock as StdOnceLock};

use super::auth::apply_auth_headers;

type ContextWindowKey = (String, String, String);
type ContextWindowCache = StdMutex<StdHashMap<ContextWindowKey, Option<usize>>>;

fn context_window_cache() -> &'static ContextWindowCache {
    static CACHE: StdOnceLock<ContextWindowCache> = StdOnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(StdHashMap::new()))
}

/// Resolve context from the owning provider catalog for hosted models, or
/// discover the configured server limit for local and uncatalogued routes.
/// Discovery is cached per (provider, base_url, model_id).
///
/// Returns `None` when neither discovery nor an applicable catalog limit is
/// available. Local routes may fall back only to an explicit runtime limit,
/// not the model's advertised architecture limit.
pub async fn fetch_provider_max_context(
    provider: &str,
    model: &str,
    api_key: &str,
) -> Option<usize> {
    let (local, catalog_window, base_url) = {
        let pdef = crate::llm_config::provider_config(provider);
        let catalog = crate::llm_config::model_catalog_entry_for_route(provider, model);
        let local = pdef
            .as_ref()
            .is_some_and(crate::llm_config::provider_is_local);
        let catalog_window = catalog.as_ref().and_then(|entry| {
            let window = if local {
                entry.runtime_context_window?
            } else {
                entry.context_window
            };
            usize::try_from(window).ok().filter(|window| *window > 0)
        });
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        (local, catalog_window, base_url)
    };
    // Read the effective catalog on every call. Overlay changes must not be
    // hidden by a discovery cache populated under an earlier configuration.
    if !local && catalog_window.is_some() {
        return catalog_window;
    }
    let cache_key = (provider.to_string(), base_url.clone(), model.to_string());

    // Fast path: cached (may be Some(n) or a cached None meaning "we tried
    // and it doesn't work for this provider, don't keep asking").
    if let Ok(cache) = context_window_cache().lock() {
        if let Some(value) = cache.get(&cache_key) {
            return value.or(catalog_window);
        }
    }

    let fetched = fetch_provider_max_context_uncached(provider, model, api_key, &base_url).await;
    if let Ok(mut cache) = context_window_cache().lock() {
        cache.insert(cache_key, fetched);
    }
    fetched.or(catalog_window)
}

/// Fetch context window from Ollama's `/api/show` endpoint.
/// Returns the num_ctx from model parameters, or the default 2048 if not set.
async fn fetch_ollama_context_window(model: &str, base_url: &str) -> Option<usize> {
    let client = crate::llm::utility_client_for_base_url(base_url);
    let url = format!("{}/api/show", base_url.trim_end_matches('/'));
    let body = serde_json::json!({"name": model});
    // Ollama is typically local — tight per-request timeout so we fail
    // fast when it isn't running.
    let response = client
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let json: serde_json::Value = response.json().await.ok()?;
    if let Some(n) = json
        .pointer("/model_info/general.context_length")
        .or_else(|| json.pointer("/model_info/context_length"))
        .and_then(|v| v.as_u64())
    {
        return Some(n as usize);
    }
    Some(
        super::ollama::OllamaRuntimeSettings::from_env_overrides_and_model(None, Some(model))
            .num_ctx as usize,
    )
}

/// Fetch context window from an OpenAI-compatible `/models` endpoint.
async fn fetch_openai_compatible_context_window(
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: &str,
) -> Option<usize> {
    let pdef = crate::llm_config::provider_config(provider);
    let client = crate::llm::utility_client_for_base_url(base_url);
    let url = pdef
        .as_ref()
        .and_then(|def| crate::llm::readiness::build_models_url(def).ok())
        .unwrap_or_else(|| format!("{}/models", base_url.trim_end_matches('/')));
    let req = client
        .get(&url)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10));
    let req = apply_auth_headers(req, api_key, pdef.as_ref());
    let response = req.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let json: serde_json::Value = response.json().await.ok()?;
    let data = json.get("data").and_then(|d| d.as_array())?;
    for entry in data {
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id != model {
            continue;
        }
        // vLLM: "max_model_len"
        if let Some(n) = entry.get("max_model_len").and_then(|v| v.as_u64()) {
            return Some(n as usize);
        }
        // Some servers: "context_length"
        if let Some(n) = entry.get("context_length").and_then(|v| v.as_u64()) {
            return Some(n as usize);
        }
        // Others: "max_context_length" / "n_ctx"
        if let Some(n) = entry.get("max_context_length").and_then(|v| v.as_u64()) {
            return Some(n as usize);
        }
        if let Some(n) = entry.get("n_ctx").and_then(|v| v.as_u64()) {
            return Some(n as usize);
        }
        // OpenRouter: top_provider.context_length
        if let Some(n) = entry
            .get("top_provider")
            .and_then(|tp| tp.get("context_length"))
            .and_then(|v| v.as_u64())
        {
            return Some(n as usize);
        }
        break;
    }
    None
}

async fn fetch_provider_max_context_uncached(
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: &str,
) -> Option<usize> {
    let caps = crate::llm::capabilities::lookup(provider, model);
    if caps.message_wire_format.is_ollama() {
        return fetch_ollama_context_window(model, base_url).await;
    }

    let endpoint = crate::llm::helpers::ResolvedProvider::resolve(provider).endpoint;
    let is_openai_compatible = endpoint.contains("/chat/completions")
        || endpoint.contains("/responses")
        || endpoint.contains("/v1/");
    if is_openai_compatible {
        return fetch_openai_compatible_context_window(provider, model, api_key, base_url).await;
    }

    None
}
