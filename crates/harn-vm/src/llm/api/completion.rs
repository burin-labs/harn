//! Text completion / fill-in-the-middle dispatch. Completion providers
//! fall into three buckets: OpenAI-style `/completions`, Ollama's
//! `/api/generate`, and a chat-based fallback that wraps the prompt in a
//! user message and re-enters the chat path.

use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::auth::apply_auth_headers;
use super::ollama::{ollama_keep_alive_override, ollama_num_ctx_override};
use super::options::LlmCallOptions;
use super::response::{extract_cache_read_tokens, extract_cache_write_tokens};
use super::result::{mock_completion_response, LlmResult};

/// Execute a text completion / fill-in-the-middle call owned by Harn.
pub(crate) async fn vm_call_completion_full(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    if opts.provider == "mock" {
        return Ok(mock_completion_response(prefix, suffix));
    }

    let resolved = crate::llm_config::provider_config(&opts.provider);
    let completion_endpoint = resolved.and_then(|p| p.completion_endpoint.as_deref());

    match completion_endpoint {
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
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let endpoint = pdef
        .and_then(|p| p.completion_endpoint.as_deref())
        .unwrap_or("/completions");

    let mut body = serde_json::json!({
        "model": opts.model,
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
    let req = apply_auth_headers(req, &opts.api_key, pdef);

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion API error: {e}",
            opts.provider
        ))))
    })?;

    let json: serde_json::Value = response.json().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion response parse error: {e}",
            opts.provider
        ))))
    })?;

    if let Some(err) = json["error"]["message"].as_str() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion API error: {err}",
            opts.provider
        )))));
    }

    Ok(LlmResult {
        text: json["choices"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        tool_calls: Vec::new(),
        input_tokens: json["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
        output_tokens: json["usage"]["completion_tokens"].as_i64().unwrap_or(0),
        cache_read_tokens: extract_cache_read_tokens(&json["usage"]),
        cache_write_tokens: extract_cache_write_tokens(&json["usage"]),
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        thinking: None,
        stop_reason: json["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| s.to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": json["choices"][0]["text"].as_str().unwrap_or(""),
            "visibility": "public",
        })],
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
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    let endpoint = pdef
        .and_then(|p| p.completion_endpoint.as_deref())
        .unwrap_or("/api/generate");

    let mut options = serde_json::Map::new();
    if let Some(num_ctx) = ollama_num_ctx_override() {
        options.insert("num_ctx".to_string(), serde_json::json!(num_ctx));
    }
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

    let mut body = serde_json::json!({
        "model": opts.model,
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
    if let Some(keep_alive) = ollama_keep_alive_override() {
        body["keep_alive"] = keep_alive;
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
    let req = apply_auth_headers(req, &opts.api_key, pdef);

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion API error: {e}",
            opts.provider
        ))))
    })?;
    let json: serde_json::Value = response.json().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion response parse error: {e}",
            opts.provider
        ))))
    })?;
    if let Some(err) = json["error"].as_str() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{} completion API error: {err}",
            opts.provider
        )))));
    }

    Ok(LlmResult {
        text: json["response"].as_str().unwrap_or("").to_string(),
        tool_calls: Vec::new(),
        input_tokens: json["prompt_eval_count"].as_i64().unwrap_or(0),
        output_tokens: json["eval_count"].as_i64().unwrap_or(0),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        thinking: None,
        stop_reason: json["done_reason"].as_str().map(|s| s.to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": json["response"].as_str().unwrap_or(""),
            "visibility": "public",
        })],
    })
}

async fn vm_call_completion_fallback(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    let mut fallback_opts = opts.clone();
    let mut instruction = String::from(
        "Continue the user's text. Return only the missing continuation with no commentary, fences, or quoting.",
    );
    if let Some(suffix) = suffix.filter(|s| !s.is_empty()) {
        instruction.push_str("\nRespect the required suffix exactly and produce only the text that belongs between PREFIX and SUFFIX.");
        fallback_opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": format!("PREFIX:\n{prefix}\n\nSUFFIX:\n{suffix}\n\nReturn only the missing text between PREFIX and SUFFIX."),
        })];
    } else {
        fallback_opts.messages = vec![serde_json::json!({
            "role": "user",
            "content": format!("PREFIX:\n{prefix}\n\nReturn only the next continuation text."),
        })];
    }
    fallback_opts.system = match &opts.system {
        Some(system) => Some(format!("{system}\n\n{instruction}")),
        None => Some(instruction),
    };
    super::vm_call_llm_full(&fallback_opts).await
}
