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

type ContextWindowKey = (String, String);
type ContextWindowCache = StdMutex<StdHashMap<ContextWindowKey, Option<usize>>>;

fn context_window_cache() -> &'static ContextWindowCache {
    static CACHE: StdOnceLock<ContextWindowCache> = StdOnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(StdHashMap::new()))
}

/// Fetch the server-reported maximum context length for a given model, if
/// available. Caches results per (base_url, model_id) so we only pay the
/// discovery cost once per session.
///
/// Returns `None` when the provider doesn't expose `/v1/models`, when the
/// model isn't found in the response, or when the request fails for any
/// reason — callers should fall back to their default threshold.
pub async fn fetch_provider_max_context(
    provider: &str,
    model: &str,
    api_key: &str,
) -> Option<usize> {
    let pdef = crate::llm_config::provider_config(provider);
    let base_url = pdef
        .as_ref()
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let cache_key = (base_url.clone(), model.to_string());

    // Fast path: cached (may be Some(n) or a cached None meaning "we tried
    // and it doesn't work for this provider, don't keep asking").
    if let Ok(cache) = context_window_cache().lock() {
        if let Some(value) = cache.get(&cache_key) {
            return *value;
        }
    }

    let fetched = fetch_provider_max_context_uncached(provider, model, api_key, &base_url).await;
    if let Ok(mut cache) = context_window_cache().lock() {
        cache.insert(cache_key, fetched);
    }
    fetched
}

/// Hardcoded context window sizes for well-known model families where the
/// provider API doesn't expose this information (Anthropic, OpenAI).
/// Returns `None` for unknown models — callers fall through to API discovery.
fn known_model_context_window(model: &str) -> Option<usize> {
    if model.starts_with("claude-") {
        return Some(200_000);
    }
    if model.starts_with("gpt-4o") || model.starts_with("gpt-4.1") || model.starts_with("chatgpt-")
    {
        return Some(128_000);
    }
    if model.starts_with("gpt-4-turbo")
        || model == "gpt-4-0125-preview"
        || model == "gpt-4-1106-preview"
    {
        return Some(128_000);
    }
    if model.starts_with("gpt-4") {
        return Some(8_192);
    }
    if model.starts_with("gpt-3.5-turbo") {
        return Some(16_385);
    }
    if model.starts_with("o1") || model.starts_with("o3") || model.starts_with("o4") {
        return Some(200_000);
    }
    if model.contains("gemini-2") || model.contains("gemini-1.5") {
        return Some(1_000_000);
    }
    if model.contains("gemini") {
        return Some(128_000);
    }
    None
}

/// Fetch context window from Ollama's `/api/show` endpoint.
/// Returns the num_ctx from model parameters, or the default 2048 if not set.
async fn fetch_ollama_context_window(model: &str, base_url: &str) -> Option<usize> {
    let client = crate::llm::shared_utility_client();
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
    Some(super::ollama::ollama_runtime_settings_from_env().num_ctx as usize)
}

/// Fetch context window from an OpenAI-compatible `/models` endpoint.
async fn fetch_openai_compatible_context_window(
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: &str,
) -> Option<usize> {
    let pdef = crate::llm_config::provider_config(provider);
    let client = crate::llm::shared_utility_client();
    let url = pdef
        .as_ref()
        .and_then(|def| super::readiness::build_models_url(def).ok())
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
    if let Some(n) = known_model_context_window(model) {
        return Some(n);
    }

    if provider == "ollama" {
        return fetch_ollama_context_window(model, base_url).await;
    }

    let is_openai_compatible = matches!(
        provider,
        "local"
            | "openai"
            | "mlx"
            | "vllm"
            | "groq"
            | "together"
            | "openrouter"
            | "deepinfra"
            | "fireworks"
            | "huggingface"
    );
    if is_openai_compatible {
        return fetch_openai_compatible_context_window(provider, model, api_key, base_url).await;
    }

    None
}

/// Derive an effective auto-compact token threshold from the server-reported
/// max context window, applying a safety margin so the compaction fires
/// *before* the request actually overflows. Returns `None` if no server value
/// is available — callers should keep their default.
///
/// The 0.75 safety factor is deliberate: the input messages alone aren't the
/// whole request (the response also needs token budget), and our token
/// estimator uses a rough chars/4 heuristic that under-counts ~15-25% on
/// English code. 0.75 gives us headroom for both without wasting too much.
pub(crate) fn effective_threshold_from_max_context(max_context: usize) -> usize {
    let bounded = max_context.max(4_096);
    (bounded * 3) / 4
}

/// Apply discovered context-window information to an auto-compact config.
///
/// Sets up two-tier compaction thresholds based on the model's actual context
/// window:
///   - Tier-1 threshold: stays at configured value unless it would overflow.
///   - Tier-2 hard limit: set to 75% of max context (the real overflow boundary).
///
/// Only lowers thresholds — never raises above what the user explicitly set.
pub(crate) async fn adapt_auto_compact_to_provider(
    ac: &mut crate::orchestration::AutoCompactConfig,
    user_specified_threshold: bool,
    user_specified_hard_limit: bool,
    provider: &str,
    model: &str,
    api_key: &str,
) {
    let Some(max_ctx) = fetch_provider_max_context(provider, model, api_key).await else {
        return;
    };
    let effective = effective_threshold_from_max_context(max_ctx);

    // Tier-2 hard limit comes from the actual context window.
    if !user_specified_hard_limit {
        ac.hard_limit_tokens = Some(effective);
    } else if let Some(ref mut hl) = ac.hard_limit_tokens {
        // Clamp user's hard limit down if it would overflow.
        if *hl > effective {
            *hl = effective;
        }
    }

    // Tier-1: clamp to the hard limit (which would make tier-1 pointless)
    // or to 65% of max context, whichever is lower.
    if user_specified_threshold {
        if ac.token_threshold > effective {
            ac.token_threshold = effective;
        }
    } else {
        let tier1_from_context = (max_ctx * 13) / 20; // 65%
        if ac.token_threshold > tier1_from_context {
            ac.token_threshold = tier1_from_context;
        }
    }
}
