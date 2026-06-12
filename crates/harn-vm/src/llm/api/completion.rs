//! Text completion / fill-in-the-middle dispatch. Completion providers
//! fall into three buckets: OpenAI-style `/completions`, Ollama's
//! `/api/generate`, and a chat-based fallback that wraps the prompt in a
//! user message and re-enters the chat path.

use std::collections::BTreeMap;

use crate::value::{VmError, VmValue};

use super::auth::apply_auth_headers;
use super::options::LlmCallOptions;
use super::response::{
    extract_cache_read_tokens, extract_cache_write_tokens, extract_openai_choice_logprobs,
};
use super::result::{mock_completion_response, LlmResult};
use super::telemetry::{source as telemetry_source, ProviderTelemetry};

async fn completion_json_response(
    provider: &str,
    response: reqwest::Response,
) -> Result<serde_json::Value, VmError> {
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = super::retry_after_header(response.headers());
        let body = response.text().await.unwrap_or_default();
        let message =
            super::classify_provider_http_error(provider, status, retry_after.as_deref(), &body)
                .message;
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            message,
        ))));
    }

    response.json().await.map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "{provider} completion response parse error: {e}"
        ))))
    })
}

/// Execute a text completion / fill-in-the-middle call owned by Harn.
pub(crate) async fn vm_call_completion_full(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    if opts.provider == "mock" {
        return Ok(mock_completion_response(prefix, suffix));
    }

    crate::llm::ensure_real_llm_allowed(&opts.provider)?;

    let resolved = crate::llm_config::provider_config(&opts.provider);
    let completion_endpoint = resolved.and_then(|p| p.completion_endpoint);

    match completion_endpoint.as_deref() {
        Some("/api/generate") => vm_call_completion_ollama(opts, prefix, suffix).await,
        Some(_) => vm_call_completion_openai_style(opts, prefix, suffix).await,
        None => vm_call_completion_fallback(opts, prefix, suffix).await,
    }
}

async fn vm_call_completion_openai_style(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    let llm_timeout = opts.resolve_timeout();
    let client = crate::llm::shared_blocking_client().clone();

    let pdef = crate::llm_config::provider_config(&opts.provider);
    let base_url = pdef
        .as_ref()
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let endpoint = pdef
        .as_ref()
        .and_then(|p| p.completion_endpoint.as_deref())
        .unwrap_or("/completions");

    let wire_model = crate::llm_config::wire_model_id(&opts.model);
    let mut body = serde_json::json!({
        "model": wire_model,
        "prompt": prefix,
        "max_tokens": opts.max_tokens,
    });
    if let Some(suffix) = suffix.filter(|s| !s.is_empty()) {
        body["suffix"] = serde_json::json!(suffix);
    }
    if let Some(temp) = opts.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(top_p) = opts.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    if opts.logprobs {
        body["logprobs"] = serde_json::json!(opts.top_logprobs.unwrap_or(0).max(0));
    }
    if let Some(stop) = &opts.stop {
        body["stop"] = serde_json::json!(stop);
    }
    if let Some(seed) = opts.seed {
        body["seed"] = serde_json::json!(seed);
    }
    if let Some(overrides) = &opts.provider_overrides {
        if let Some(obj) = overrides.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
    }

    let req = client
        .post(format!("{base_url}{endpoint}"))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .json(&body);
    let req = apply_auth_headers(req, &opts.api_key, pdef.as_ref());

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "{} completion API error: {e}",
            opts.provider
        ))))
    })?;

    let json = completion_json_response(&opts.provider, response).await?;

    if let Some(err) = json["error"]["message"].as_str() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{} completion API error: {err}", opts.provider),
        ))));
    }

    let request_id = json["id"].as_str().filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_openai_usage(&json["usage"], request_id);
    Ok(LlmResult {
        served_fast: false,
        text: json["choices"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        tool_calls: Vec::new(),
        input_tokens: json["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["usage"]["completion_tokens"].as_i64().unwrap_or(0),
        cache_read_tokens: extract_cache_read_tokens(&json["usage"]),
        cache_write_tokens: extract_cache_write_tokens(&json["usage"]),
        cache_supported: true,
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        thinking: None,
        thinking_summary: None,
        stop_reason: json["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| s.to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": json["choices"][0]["text"].as_str().unwrap_or(""),
            "visibility": "public",
        })],
        logprobs: extract_openai_choice_logprobs(&json["choices"][0]),
        telemetry,
    })
}

async fn vm_call_completion_ollama(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    let llm_timeout = opts.resolve_timeout();
    let client = crate::llm::shared_blocking_client().clone();
    let pdef = crate::llm_config::provider_config(&opts.provider);
    let base_url = pdef
        .as_ref()
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    let endpoint = pdef
        .as_ref()
        .and_then(|p| p.completion_endpoint.as_deref())
        .unwrap_or("/api/generate");

    let mut options = serde_json::Map::new();
    if let Some(temp) = opts.temperature {
        options.insert("temperature".to_string(), serde_json::json!(temp));
    }
    if let Some(top_p) = opts.top_p {
        options.insert("top_p".to_string(), serde_json::json!(top_p));
    }
    if let Some(top_k) = opts.top_k {
        options.insert("top_k".to_string(), serde_json::json!(top_k));
    }
    if let Some(seed) = opts.seed {
        options.insert("seed".to_string(), serde_json::json!(seed));
    }
    if let Some(stop) = &opts.stop {
        options.insert("stop".to_string(), serde_json::json!(stop));
    }
    options.insert(
        "num_predict".to_string(),
        serde_json::json!(opts.max_tokens),
    );

    let wire_model = crate::llm_config::wire_model_id(&opts.model);
    let mut body = serde_json::json!({
        "model": wire_model,
        "prompt": prefix,
        "stream": false,
        "raw": true,
        "options": options,
    });
    if let Some(suffix) = suffix.filter(|s| !s.is_empty()) {
        body["suffix"] = serde_json::json!(suffix);
    }
    if let Some(system) = &opts.system {
        body["system"] = serde_json::json!(system);
    }
    super::apply_ollama_runtime_settings(&mut body, opts.provider_overrides.as_ref());

    let req = client
        .post(format!("{base_url}{endpoint}"))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .json(&body);
    let req = apply_auth_headers(req, &opts.api_key, pdef.as_ref());

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "{} completion API error: {e}",
            opts.provider
        ))))
    })?;
    let json = completion_json_response(&opts.provider, response).await?;
    if let Some(err) = json["error"].as_str() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("{} completion API error: {err}", opts.provider),
        ))));
    }

    let telemetry = ProviderTelemetry::from_ollama_done(&json, telemetry_source::OLLAMA_GENERATE);
    Ok(LlmResult {
        served_fast: false,
        text: json["response"].as_str().unwrap_or("").to_string(),
        tool_calls: Vec::new(),
        input_tokens: json["prompt_eval_count"].as_i64().unwrap_or(0),
        output_tokens: json["eval_count"].as_i64().unwrap_or(0),
        // Native Ollama generate responses carry no cache field; 0 is
        // "unknown", not a real miss. `cache_supported: false` keeps the cost
        // layer from scoring a local model as a 100% cache miss.
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: false,
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        thinking: None,
        thinking_summary: None,
        stop_reason: json["done_reason"].as_str().map(|s| s.to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": json["response"].as_str().unwrap_or(""),
            "visibility": "public",
        })],
        logprobs: Vec::new(),
        telemetry,
    })
}

async fn vm_call_completion_fallback(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    let mut fallback_opts = opts.clone();
    let has_suffix = suffix.is_some_and(|s| !s.is_empty());
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "prefix".to_string(),
        VmValue::String(std::sync::Arc::from(prefix)),
    );
    bindings.insert("has_suffix".to_string(), VmValue::Bool(has_suffix));
    bindings.insert(
        "suffix".to_string(),
        VmValue::String(std::sync::Arc::from(suffix.unwrap_or_default())),
    );
    let instruction = crate::stdlib::template::render_stdlib_prompt_asset(
        "llm/prompts/completion_fallback_system.harn.prompt",
        Some(&bindings),
    )?;
    let user_prompt = crate::stdlib::template::render_stdlib_prompt_asset(
        "llm/prompts/completion_fallback_user.harn.prompt",
        Some(&bindings),
    )?;
    fallback_opts.messages = vec![serde_json::json!({
        "role": "user",
        "content": user_prompt,
    })];
    fallback_opts.system = match &opts.system {
        Some(system) => Some(format!("{system}\n\n{instruction}")),
        None => Some(instruction),
    };
    super::vm_call_llm_full(&fallback_opts).await
}
