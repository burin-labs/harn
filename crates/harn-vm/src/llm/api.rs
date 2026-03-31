use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, save_fixture, LlmReplayMode,
};

/// Sender for streaming text deltas from an in-flight LLM call.
pub(crate) type DeltaSender = tokio::sync::mpsc::UnboundedSender<String>;

// =============================================================================
// LLM call options -- single struct replaces 12+ positional parameters
// =============================================================================

/// Extended thinking configuration.
#[derive(Clone, Debug)]
pub(crate) enum ThinkingConfig {
    /// Enable with provider defaults.
    Enabled,
    /// Enable with a specific token budget.
    WithBudget(i64),
}

/// All options for an LLM API call, extracted once from user-facing args.
#[derive(Clone)]
pub(crate) struct LlmCallOptions {
    // --- Routing ---
    pub provider: String,
    pub model: String,
    pub api_key: String,

    // --- Conversation ---
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub transcript_id: Option<String>,
    pub transcript_summary: Option<String>,
    pub transcript_metadata: Option<serde_json::Value>,

    // --- Generation ---
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,

    // --- Structured output ---
    pub response_format: Option<String>,
    pub json_schema: Option<serde_json::Value>,

    // --- Thinking ---
    pub thinking: Option<ThinkingConfig>,

    // --- Tools ---
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,

    // --- Caching ---
    pub cache: bool,

    // --- Advanced ---
    pub timeout: Option<u64>,

    // --- Provider-specific overrides ---
    pub provider_overrides: Option<serde_json::Value>,
}

// =============================================================================
// LLM response type
// =============================================================================

pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: String,
    pub provider: String,
    pub thinking: Option<String>,
    pub stop_reason: Option<String>,
    pub blocks: Vec<serde_json::Value>,
}

pub(crate) fn vm_build_llm_result(
    result: &LlmResult,
    parsed_json: Option<VmValue>,
    transcript: Option<VmValue>,
) -> VmValue {
    use crate::stdlib::json_to_vm_value;

    let mut dict = BTreeMap::new();
    dict.insert(
        "text".to_string(),
        VmValue::String(Rc::from(result.text.as_str())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(Rc::from(result.model.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(result.provider.as_str())),
    );
    dict.insert(
        "input_tokens".to_string(),
        VmValue::Int(result.input_tokens),
    );
    dict.insert(
        "output_tokens".to_string(),
        VmValue::Int(result.output_tokens),
    );

    if let Some(json_val) = parsed_json {
        dict.insert("data".to_string(), json_val);
    }

    if !result.tool_calls.is_empty() {
        let calls: Vec<VmValue> = result.tool_calls.iter().map(json_to_vm_value).collect();
        dict.insert("tool_calls".to_string(), VmValue::List(Rc::new(calls)));
    }

    if let Some(ref thinking) = result.thinking {
        dict.insert(
            "thinking".to_string(),
            VmValue::String(Rc::from(thinking.as_str())),
        );
        dict.insert(
            "private_reasoning".to_string(),
            VmValue::String(Rc::from(thinking.as_str())),
        );
    }

    if let Some(ref stop_reason) = result.stop_reason {
        dict.insert(
            "stop_reason".to_string(),
            VmValue::String(Rc::from(stop_reason.as_str())),
        );
    }

    if let Some(transcript) = transcript {
        dict.insert("transcript".to_string(), transcript);
    }

    dict.insert(
        "visible_text".to_string(),
        VmValue::String(Rc::from(result.text.as_str())),
    );
    dict.insert(
        "blocks".to_string(),
        VmValue::List(Rc::new(
            result
                .blocks
                .iter()
                .map(json_to_vm_value)
                .collect::<Vec<_>>(),
        )),
    );

    VmValue::Dict(Rc::new(dict))
}

// =============================================================================
// Core LLM call with all options
// =============================================================================

/// Execute an LLM call (non-streaming).
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(opts, None).await
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(opts, Some(delta_tx)).await
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

    let resolved = crate::llm_config::provider_config(&opts.provider);
    let completion_endpoint = resolved.and_then(|p| p.completion_endpoint.as_deref());

    match completion_endpoint {
        Some("/api/generate") => vm_call_completion_ollama(opts, prefix, suffix).await,
        Some(_) => vm_call_completion_openai_style(opts, prefix, suffix).await,
        None => vm_call_completion_fallback(opts, prefix, suffix).await,
    }
}

async fn vm_call_llm_full_inner(
    opts: &LlmCallOptions,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    // Mock provider: return deterministic response without API call.
    if opts.provider == "mock" {
        return Ok(mock_llm_response(
            &opts.messages,
            opts.system.as_deref(),
            opts.native_tools.as_deref(),
        ));
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(&opts.model, &opts.messages, opts.system.as_deref());

    // In replay mode, return cached fixture
    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        )))));
    }

    let result = vm_call_llm_api(opts, delta_tx).await;

    // On failure, check for provider fallback chain
    let result = match result {
        Ok(r) => r,
        Err(primary_err) => {
            if let Some(pdef) = crate::llm_config::provider_config(&opts.provider) {
                if let Some(ref fallback_provider) = pdef.fallback {
                    let fb_key =
                        super::helpers::vm_resolve_api_key(fallback_provider).unwrap_or_default();
                    if !fb_key.is_empty() {
                        let mut fb_opts = opts.clone();
                        fb_opts.provider = fallback_provider.clone();
                        fb_opts.api_key = fb_key;
                        let fb_result = vm_call_llm_api(&fb_opts, None).await;
                        match fb_result {
                            Ok(r) => r,
                            Err(_) => return Err(primary_err),
                        }
                    } else {
                        return Err(primary_err);
                    }
                } else {
                    return Err(primary_err);
                }
            } else {
                return Err(primary_err);
            }
        }
    };

    // In record mode, save the fixture
    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }

    // Accumulate cost for budget tracking
    super::cost::accumulate_cost(&result.model, result.input_tokens, result.output_tokens)?;

    Ok(result)
}

fn mock_completion_response(prefix: &str, suffix: Option<&str>) -> LlmResult {
    let suffix = suffix.unwrap_or_default();
    let text = format!(
        "Mock completion after {} chars{}",
        prefix.chars().count(),
        if suffix.is_empty() {
            String::new()
        } else {
            format!(" before {} chars", suffix.chars().count())
        }
    );
    LlmResult {
        text: text.clone(),
        tool_calls: Vec::new(),
        input_tokens: (prefix.len() + suffix.len()) as i64,
        output_tokens: 16,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        stop_reason: Some("stop".to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": text,
            "visibility": "public",
        })],
    }
}

async fn vm_call_completion_openai_style(
    opts: &LlmCallOptions,
    prefix: &str,
    suffix: Option<&str>,
) -> Result<LlmResult, VmError> {
    let llm_timeout = opts.timeout.unwrap_or_else(|| {
        std::env::var("HARN_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
    });
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

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
    let llm_timeout = opts.timeout.unwrap_or_else(|| {
        std::env::var("HARN_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
    });
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let pdef = crate::llm_config::provider_config(&opts.provider);
    let base_url = pdef
        .map(crate::llm_config::resolve_base_url)
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    let endpoint = pdef
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
    vm_call_llm_full(&fallback_opts).await
}

async fn vm_call_llm_api(
    opts: &LlmCallOptions,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;
    let model = &opts.model;
    let streaming = delta_tx.is_some();
    let llm_timeout = opts.timeout.unwrap_or_else(|| {
        std::env::var("HARN_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
    });
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resolved = super::helpers::ResolvedProvider::resolve(provider);
    let is_ollama = provider == "ollama" || resolved.endpoint.contains("/api/chat");

    // Build request body based on API style
    let mut body = if resolved.is_anthropic_style {
        let mut body = serde_json::json!({
            "model": model,
            "messages": opts.messages,
            "max_tokens": opts.max_tokens,
        });
        if let Some(ref sys) = opts.system {
            if opts.cache {
                // Anthropic cache control: wrap system in content blocks
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": sys,
                    "cache_control": {"type": "ephemeral"},
                }]);
            } else {
                body["system"] = serde_json::json!(sys);
            }
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = opts.top_k {
            body["top_k"] = serde_json::json!(top_k);
        }
        if let Some(ref stop) = opts.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        if let Some(ref tc) = opts.tool_choice {
            body["tool_choice"] = tc.clone();
        }
        // Anthropic structured output via tool-use constraint
        if opts.response_format.as_deref() == Some("json") {
            if let Some(ref schema) = opts.json_schema {
                body["tools"] = {
                    let mut tools = body["tools"].as_array().cloned().unwrap_or_default();
                    tools.push(serde_json::json!({
                        "name": "json_response",
                        "description": "Return a structured JSON response matching the schema.",
                        "input_schema": schema,
                    }));
                    serde_json::json!(tools)
                };
                body["tool_choice"] = serde_json::json!({"type": "tool", "name": "json_response"});
            }
        }
        // Anthropic thinking
        if let Some(ref thinking) = opts.thinking {
            let budget = match thinking {
                ThinkingConfig::Enabled => 10000,
                ThinkingConfig::WithBudget(b) => *b,
            };
            body["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
        }
        body
    } else {
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned());

        let mut body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": opts.max_tokens,
        });
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(ref stop) = opts.stop {
            body["stop"] = serde_json::json!(stop);
        }
        if let Some(seed) = opts.seed {
            body["seed"] = serde_json::json!(seed);
        }
        if let Some(fp) = opts.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(fp);
        }
        if let Some(pp) = opts.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pp);
        }
        if opts.response_format.as_deref() == Some("json") {
            if let Some(ref schema) = opts.json_schema {
                body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema,
                        "strict": true,
                    }
                });
            } else {
                body["response_format"] = serde_json::json!({"type": "json_object"});
            }
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        if let Some(ref tc) = opts.tool_choice {
            body["tool_choice"] = tc.clone();
        }
        body
    };

    // Ollama thinking support
    if is_ollama {
        if let Some(ref thinking) = opts.thinking {
            body["think"] = serde_json::json!(matches!(
                thinking,
                ThinkingConfig::Enabled | ThinkingConfig::WithBudget(_)
            ));
        }
    }

    // Merge provider-specific overrides
    if let Some(ref overrides) = opts.provider_overrides {
        if let Some(obj) = overrides.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
    }

    if streaming {
        body["stream"] = serde_json::json!(true);
        // OpenAI-style: request usage in the final streaming chunk.
        if !resolved.is_anthropic_style {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
    }

    // Send request
    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .json(&body);
    let req = resolved.apply_headers(req, &opts.api_key);

    if streaming {
        let tx = delta_tx.unwrap();
        let response = req.send().await.map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} stream error: {e}"
            ))))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} HTTP {status}: {body}"
            )))));
        }
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ct.contains("text/event-stream") {
            return vm_call_llm_api_sse_from_response(response, model, &resolved, tx).await;
        }
        return vm_call_llm_api_ndjson_from_response(response, model, tx).await;
    }

    let response = req.send().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{provider} API error: {e}"
        ))))
    })?;

    let json: serde_json::Value = response.json().await.map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{provider} response parse error: {e}"
        ))))
    })?;

    parse_llm_response(&json, provider, model, &resolved)
}

/// Parse a complete (non-streaming) LLM JSON response into an `LlmResult`.
fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    resolved: &super::helpers::ResolvedProvider<'_>,
) -> Result<LlmResult, VmError> {
    if resolved.is_anthropic_style {
        let mut text = String::new();
        let mut thinking_text = String::new();
        let mut tool_calls = Vec::new();
        let mut blocks = Vec::new();

        if let Some(content) = json["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text.push_str(t);
                            blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                        }
                    }
                    Some("thinking") => {
                        if let Some(t) = block["thinking"].as_str() {
                            thinking_text.push_str(t);
                            blocks.push(serde_json::json!({"type": "reasoning", "text": t, "visibility": "private"}));
                        }
                    }
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let input = block["input"].clone();
                        tool_calls.push(serde_json::json!({
                            "id": id,
                            "name": name,
                            "arguments": input,
                        }));
                        blocks.push(serde_json::json!({
                            "type": "tool_call",
                            "id": block["id"].clone(),
                            "name": block["name"].clone(),
                            "arguments": block["input"].clone(),
                            "visibility": "internal",
                        }));
                    }
                    _ => {}
                }
            }
        }

        if text.is_empty() && tool_calls.is_empty() {
            if let Some(err) = json["error"]["message"].as_str() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {err}"
                )))));
            }
        }

        let input_tokens = json["usage"]["input_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["output_tokens"].as_i64().unwrap_or(0);
        let stop_reason = json["stop_reason"].as_str().map(|s| s.to_string());

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: if thinking_text.is_empty() {
                None
            } else {
                Some(thinking_text)
            },
            stop_reason,
            blocks,
        })
    } else {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let mut blocks = if text.is_empty() {
            Vec::new()
        } else {
            vec![serde_json::json!({"type": "output_text", "text": text, "visibility": "public"})]
        };

        let mut tool_calls = Vec::new();
        if let Some(calls) = json["choices"][0]["message"]["tool_calls"].as_array() {
            for call in calls {
                let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: serde_json::Value =
                    serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
                let id = call["id"].as_str().unwrap_or("").to_string();
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": arguments,
                }));
                blocks.push(serde_json::json!({
                    "type": "tool_call",
                    "id": call["id"].clone(),
                    "name": call["function"]["name"].clone(),
                    "arguments": serde_json::from_str::<serde_json::Value>(args_str).unwrap_or(serde_json::json!({})),
                    "visibility": "internal",
                }));
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        let stop_reason = json["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| s.to_string());

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: None,
            stop_reason,
            blocks,
        })
    }
}

/// Consume an SSE streaming response from an already-sent request.
/// Parses `data: {...}` lines from the response body.
async fn vm_call_llm_api_sse_from_response(
    response: reqwest::Response,
    model: &str,
    resolved: &super::helpers::ResolvedProvider<'_>,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|r| r.map_err(std::io::Error::other)),
    ));
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();

    // Anthropic tool-use streaming state
    struct ToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut current_tool: Option<ToolBlock> = None;
    let mut thinking_text = String::new();
    let mut in_thinking_block = false;
    let mut stop_reason: Option<String> = None;

    // OpenAI tool-call streaming state
    let mut oai_tool_map: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();

    while let Ok(Some(line)) = lines.next_line().await {
        // SSE lines are prefixed with "data: "
        let data = if let Some(d) = line.strip_prefix("data: ") {
            d
        } else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if resolved.is_anthropic_style {
            match json["type"].as_str() {
                Some("message_start") => {
                    if let Some(n) = json["message"]["usage"]["input_tokens"].as_i64() {
                        input_tokens = n;
                    }
                }
                Some("content_block_start") => {
                    let block = &json["content_block"];
                    match block["type"].as_str() {
                        Some("tool_use") => {
                            current_tool = Some(ToolBlock {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                input_json: String::new(),
                            });
                        }
                        Some("thinking") => {
                            in_thinking_block = true;
                        }
                        _ => {}
                    }
                }
                Some("content_block_delta") => {
                    let delta = &json["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(t) = delta["text"].as_str() {
                                text.push_str(t);
                                let _ = delta_tx.send(t.to_string());
                                blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta["thinking"].as_str() {
                                thinking_text.push_str(t);
                                blocks.push(serde_json::json!({"type": "reasoning", "text": t, "visibility": "private"}));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(ref mut tool) = current_tool {
                                if let Some(j) = delta["partial_json"].as_str() {
                                    tool.input_json.push_str(j);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("content_block_stop") => {
                    if let Some(tool) = current_tool.take() {
                        let args = serde_json::from_str::<serde_json::Value>(&tool.input_json)
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        tool_calls.push(serde_json::json!({
                            "id": tool.id, "name": tool.name, "arguments": args,
                        }));
                        blocks.push(serde_json::json!({"type": "tool_call", "id": tool.id, "name": tool.name, "arguments": args, "visibility": "internal"}));
                    }
                    in_thinking_block = false;
                }
                Some("message_delta") => {
                    if let Some(n) = json["usage"]["output_tokens"].as_i64() {
                        output_tokens = n;
                    }
                    if let Some(sr) = json["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(sr.to_string());
                    }
                }
                _ => {}
            }
        } else {
            // OpenAI-style streaming
            let choice = &json["choices"][0];
            let delta = &choice["delta"];

            if let Some(content) = delta["content"].as_str() {
                text.push_str(content);
                let _ = delta_tx.send(content.to_string());
                blocks.push(serde_json::json!({"type": "output_text", "text": content, "visibility": "public"}));
            }

            // Capture finish_reason
            if let Some(fr) = choice["finish_reason"].as_str() {
                stop_reason = Some(fr.to_string());
            }

            // Tool calls
            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let entry = oai_tool_map.entry(idx).or_insert_with(|| {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                        (id, name, String::new())
                    });
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        entry.2.push_str(args);
                    }
                }
            }

            // Usage in final chunk
            if let Some(usage) = json.get("usage") {
                if let Some(n) = usage["prompt_tokens"].as_i64() {
                    input_tokens = n;
                }
                if let Some(n) = usage["completion_tokens"].as_i64() {
                    output_tokens = n;
                }
            }
        }
    }

    // Finalize OpenAI tool calls
    for (_, (id, name, args_str)) in oai_tool_map {
        let args = serde_json::from_str::<serde_json::Value>(&args_str)
            .unwrap_or(serde_json::Value::Object(Default::default()));
        tool_calls.push(serde_json::json!({
            "id": id, "name": name, "arguments": args,
        }));
        blocks.push(serde_json::json!({"type": "tool_call", "id": id, "name": name, "arguments": args, "visibility": "internal"}));
    }

    let _ = in_thinking_block; // suppress unused warning

    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        model: model.to_string(),
        provider: if resolved.is_anthropic_style {
            "anthropic".to_string()
        } else {
            "openai".to_string()
        },
        thinking: if thinking_text.is_empty() {
            None
        } else {
            Some(thinking_text)
        },
        stop_reason,
        blocks,
    })
}

/// Apply auth headers to a request based on provider config.
pub(crate) fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
    pdef: Option<&crate::llm_config::ProviderDef>,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        return req;
    }
    if let Some(p) = pdef {
        match p.auth_style.as_str() {
            "header" => {
                let header_name = p.auth_header.as_deref().unwrap_or("x-api-key");
                req.header(header_name, api_key)
            }
            "bearer" => req.header("Authorization", format!("Bearer {api_key}")),
            "none" => req,
            _ => req.header("Authorization", format!("Bearer {api_key}")),
        }
    } else {
        // Unknown provider: default to bearer
        req.header("Authorization", format!("Bearer {api_key}"))
    }
}

/// Consume an NDJSON streaming response, forwarding text deltas to `delta_tx`
/// while accumulating the full result.
///
/// Supports Ollama format (one JSON object per line):
/// `{"message":{"role":"assistant","content":"Hi"},"done":false}`
/// Final line has `"done":true` with token counts.
///
/// Also supports OpenAI-compatible NDJSON where each line is `data: {...}`.
async fn vm_call_llm_api_ndjson_from_response(
    response: reqwest::Response,
    model: &str,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|r| r.map_err(std::io::Error::other)),
    ));
    let mut lines = reader.lines();

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut result_model = model.to_string();

    let mut thinking_text = String::new();
    let mut blocks = Vec::new();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract text delta from message.content or message.thinking (Qwen 3 extended thinking)
        let content = json["message"]["content"].as_str().unwrap_or("");
        let thinking = json["message"]["thinking"].as_str().unwrap_or("");
        if !content.is_empty() {
            text.push_str(content);
            let _ = delta_tx.send(content.to_string());
            blocks.push(
                serde_json::json!({"type": "output_text", "text": content, "visibility": "public"}),
            );
        } else if !thinking.is_empty() {
            thinking_text.push_str(thinking);
            let _ = delta_tx.send(thinking.to_string());
            blocks.push(
                serde_json::json!({"type": "reasoning", "text": thinking, "visibility": "private"}),
            );
        }

        if let Some(m) = json["model"].as_str() {
            result_model = m.to_string();
        }

        // Final chunk has done=true with token counts
        if json["done"].as_bool() == Some(true) {
            if let Some(n) = json["prompt_eval_count"].as_i64() {
                input_tokens = n;
            }
            if let Some(n) = json["eval_count"].as_i64() {
                output_tokens = n;
            }
            break;
        }
    }

    // Include thinking text in the visible text if the model only produced thinking tokens
    let thinking = if thinking_text.is_empty() {
        None
    } else {
        Some(thinking_text.clone())
    };
    if text.is_empty() && !thinking_text.is_empty() {
        text = thinking_text;
    }

    Ok(LlmResult {
        text,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens,
        model: result_model,
        provider: "ollama".to_string(),
        thinking,
        stop_reason: None,
        blocks,
    })
}
