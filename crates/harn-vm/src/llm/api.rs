use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, save_fixture, LlmReplayMode,
};
use super::provider::LlmProvider;

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
    pub output_schema: Option<serde_json::Value>,
    pub output_validation: Option<String>,

    // --- Thinking ---
    pub thinking: Option<ThinkingConfig>,

    // --- Tools ---
    pub tools: Option<VmValue>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,

    // --- Caching ---
    pub cache: bool,

    // --- Transport ---
    pub timeout: Option<u64>,
    /// Per-chunk idle timeout for streaming responses (seconds).
    pub idle_timeout: Option<u64>,
    /// When true, use streaming SSE transport (token-by-token deltas).
    /// When false, use synchronous request/response. Default: true.
    pub stream: bool,

    // --- Provider-specific overrides ---
    pub provider_overrides: Option<serde_json::Value>,
}

/// Resolve effective request timeout: explicit value > `HARN_LLM_TIMEOUT` env > 120s default.
fn resolve_timeout(explicit: Option<u64>) -> u64 {
    explicit.unwrap_or_else(|| {
        std::env::var("HARN_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
    })
}

impl LlmCallOptions {
    pub(crate) fn resolve_timeout(&self) -> u64 {
        resolve_timeout(self.timeout)
    }
}

/// Send-safe subset of `LlmCallOptions` used for provider transport.
#[derive(Clone, Debug)]
pub(crate) struct LlmRequestPayload {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub response_format: Option<String>,
    pub json_schema: Option<serde_json::Value>,
    pub thinking: Option<ThinkingConfig>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub cache: bool,
    pub timeout: Option<u64>,
    pub stream: bool,
    pub provider_overrides: Option<serde_json::Value>,
}

impl LlmRequestPayload {
    pub(crate) fn resolve_timeout(&self) -> u64 {
        resolve_timeout(self.timeout)
    }
}

impl From<&LlmCallOptions> for LlmRequestPayload {
    fn from(opts: &LlmCallOptions) -> Self {
        Self {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            api_key: opts.api_key.clone(),
            messages: opts.messages.clone(),
            system: opts.system.clone(),
            max_tokens: opts.max_tokens,
            temperature: opts.temperature,
            top_p: opts.top_p,
            top_k: opts.top_k,
            stop: opts.stop.clone(),
            seed: opts.seed,
            frequency_penalty: opts.frequency_penalty,
            presence_penalty: opts.presence_penalty,
            response_format: opts.response_format.clone(),
            json_schema: opts.json_schema.clone(),
            thinking: opts.thinking.clone(),
            native_tools: opts.native_tools.clone(),
            tool_choice: opts.tool_choice.clone(),
            cache: opts.cache,
            timeout: opts.timeout,
            stream: opts.stream,
            provider_overrides: opts.provider_overrides.clone(),
        }
    }
}

// =============================================================================
// LLM response type
// =============================================================================

pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Prompt tokens served from the provider's cache (when supported).
    /// Anthropic: `usage.cache_read_input_tokens`.
    /// OpenAI: `usage.prompt_tokens_details.cached_tokens`.
    /// OpenRouter passthrough for Anthropic: `usage.cache_read_input_tokens`.
    /// Defaults to 0 when the provider doesn't report it.
    pub cache_read_tokens: i64,
    /// Prompt tokens written to the provider's cache on this request
    /// (Anthropic `usage.cache_creation_input_tokens`). Helps distinguish
    /// "warm-up" calls from cache hits.
    pub cache_write_tokens: i64,
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
    tools_val: Option<&VmValue>,
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
    // Cache accounting (0 when provider doesn't report cache info).
    dict.insert(
        "cache_read_tokens".to_string(),
        VmValue::Int(result.cache_read_tokens),
    );
    dict.insert(
        "cache_write_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
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

    // `visible_text` is the prose the model wrote — the same text the user
    // would see in a chat bubble — with any fenceless TS tool-call
    // expressions stripped out. Tool calls are structured data in the
    // `tool_calls` field and should never appear as narration. When the
    // caller did not supply a tool registry OR the model used provider-
    // native tool calls (so the text contains no call expressions), this
    // equals `text` verbatim. Agent_loop applies the same semantics on its
    // final iteration — the two interfaces are intentionally symmetric.
    let visible_text = if tools_val.is_some() && result.tool_calls.is_empty() {
        let parse_result = super::tools::parse_text_tool_calls_with_tools(&result.text, tools_val);
        parse_result.prose
    } else {
        result.text.clone()
    };
    dict.insert(
        "visible_text".to_string(),
        VmValue::String(Rc::from(visible_text.as_str())),
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

/// Execute an LLM call. Internally always uses the streaming path with a
/// discarding receiver so all callers go through a single code path with
/// consistent HTTP status handling, error detection, and provider semantics.
/// Callers that don't care about token-level deltas just let the channel
/// buffer and drop the receiver on return — negligible cost.
pub(crate) async fn vm_call_llm_full(opts: &LlmCallOptions) -> Result<LlmResult, VmError> {
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    vm_call_llm_full_inner(opts, Some(delta_tx)).await
}

/// Execute an LLM call, streaming text deltas to `delta_tx`.
pub(crate) async fn vm_call_llm_full_streaming(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(opts, Some(delta_tx)).await
}

/// Execute provider I/O on Tokio's multithreaded scheduler while keeping
/// VM-local values and transcript assembly on the caller's LocalSet.
pub(crate) async fn vm_call_llm_full_streaming_offthread(
    opts: &LlmCallOptions,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    let request = LlmRequestPayload::from(opts);
    tokio::task::spawn(
        async move { vm_call_llm_full_inner_offthread(&request, Some(delta_tx)).await },
    )
    .await
    .map_err(|join_err| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "llm_call background task failed: {join_err}"
        ))))
    })?
    .map_err(|message| VmError::Thrown(VmValue::String(Rc::from(message))))
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
    let request = LlmRequestPayload::from(opts);
    vm_call_llm_full_inner_request(&request, delta_tx).await
}

async fn vm_call_llm_full_inner_request(
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    // Mock provider: return deterministic response without API call.
    if request.provider == "mock" {
        return Ok(mock_llm_response(
            &request.messages,
            request.system.as_deref(),
            request.native_tools.as_deref(),
        ));
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());

    // In replay mode, return cached fixture
    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        )))));
    }

    let result = vm_call_llm_api(request, delta_tx).await;

    // On failure, check for provider fallback chain without carrying the VM
    // error across the off-thread await boundary.
    let primary_message = result.as_ref().err().map(ToString::to_string);
    let result = match (result, primary_message) {
        (Ok(r), _) => r,
        (Err(_), Some(message)) => try_fallback_provider(request, message)
            .await
            .map_err(|msg| VmError::Thrown(VmValue::String(Rc::from(msg))))?,
        (Err(_), None) => unreachable!("error branch must capture a message"),
    };

    // In record mode, save the fixture
    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }

    // Accumulate cost for budget tracking
    super::cost::accumulate_cost(&result.model, result.input_tokens, result.output_tokens)?;

    Ok(result)
}

async fn vm_call_llm_full_inner_offthread(
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, String> {
    if request.provider == "mock" {
        return Ok(mock_llm_response(
            &request.messages,
            request.system.as_deref(),
            request.native_tools.as_deref(),
        ));
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(&request.model, &request.messages, request.system.as_deref());

    if replay_mode == LlmReplayMode::Replay {
        return load_fixture(&hash).ok_or_else(|| {
            format!("No fixture found for LLM call (hash: {hash}). Run with --record first.")
        });
    }

    let result = vm_call_llm_api(request, delta_tx)
        .await
        .map_err(|err| err.to_string());
    let result = match result {
        Ok(result) => result,
        Err(message) => try_fallback_provider(request, message).await?,
    };

    if replay_mode == LlmReplayMode::Record {
        save_fixture(&hash, &result);
    }

    super::cost::accumulate_cost(&result.model, result.input_tokens, result.output_tokens)
        .map_err(|err| err.to_string())?;

    Ok(result)
}

/// Attempt the request on the configured fallback provider.  Returns the
/// original `primary_message` as the error if no fallback is available or
/// the fallback also fails.
async fn try_fallback_provider(
    request: &LlmRequestPayload,
    primary_message: String,
) -> Result<LlmResult, String> {
    let Some(pdef) = crate::llm_config::provider_config(&request.provider) else {
        return Err(primary_message);
    };
    let Some(ref fallback_provider) = pdef.fallback else {
        return Err(primary_message);
    };

    let fb_key = super::helpers::resolve_api_key(fallback_provider).unwrap_or_default();
    if fb_key.is_empty() {
        return Err(primary_message);
    }

    let mut fb_request = request.clone();
    fb_request.provider = fallback_provider.clone();
    fb_request.api_key = fb_key;
    vm_call_llm_api(&fb_request, None)
        .await
        .map_err(|_| primary_message)
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
        cache_read_tokens: 0,
        cache_write_tokens: 0,
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
    let llm_timeout = opts.resolve_timeout();
    let client = super::shared_blocking_client().clone();

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
    let client = super::shared_blocking_client().clone();
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
    vm_call_llm_full(&fallback_opts).await
}

fn render_openai_message_content_as_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => {
            let mut rendered = String::new();
            for block in blocks {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" | "output_text" => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            rendered.push_str(text);
                        }
                    }
                    "tool_result" => {
                        let content = block
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if !rendered.is_empty() {
                            rendered.push_str("\n\n");
                        }
                        rendered.push_str("[Result] ");
                        rendered.push_str(content);
                    }
                    "reasoning" | "thinking" => {
                        if let Some(text) = block
                            .get("text")
                            .and_then(|v| v.as_str())
                            .or_else(|| block.get("thinking").and_then(|v| v.as_str()))
                        {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(text);
                        }
                    }
                    _ => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(text);
                        } else if !block.is_null() {
                            if !rendered.is_empty() {
                                rendered.push('\n');
                            }
                            rendered.push_str(&block.to_string());
                        }
                    }
                }
            }
            rendered
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn extract_openai_message_field_as_text(
    message: &serde_json::Value,
    field_names: &[&str],
) -> String {
    let mut combined = String::new();
    for field_name in field_names {
        let field_text = message
            .get(*field_name)
            .map(render_openai_message_content_as_text)
            .unwrap_or_default();
        if field_text.trim().is_empty() {
            continue;
        }
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(field_text.trim());
    }
    combined
}

fn append_paragraph(target: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text.trim());
}

fn normalize_openai_message_text(message: &serde_json::Value) -> (String, String) {
    let raw_text = extract_openai_message_field_as_text(message, &["content"]);
    let reasoning_text =
        extract_openai_message_field_as_text(message, &["reasoning", "reasoning_content"]);
    // Split `<think>...</think>` blocks out of the content so the agent
    // loop doesn't interpret reasoning tokens as its own output or try to
    // parse tool calls inside them. Qwen3/Qwen3.5 emit these inline when
    // `chat_template_kwargs.enable_thinking` is set.
    let (mut text, inline_thinking) = split_openai_thinking_blocks(&raw_text);
    let mut extracted_thinking = String::new();
    append_paragraph(&mut extracted_thinking, &reasoning_text);
    append_paragraph(&mut extracted_thinking, &inline_thinking);
    if text.is_empty() && !extracted_thinking.is_empty() {
        text = extracted_thinking.clone();
    }
    (text, extracted_thinking)
}

pub(crate) fn normalize_openai_style_messages(
    messages: Vec<serde_json::Value>,
    force_string_content: bool,
) -> Vec<serde_json::Value> {
    messages
        .into_iter()
        .map(|message| {
            let Some(object) = message.as_object() else {
                return message;
            };
            let mut normalized = object.clone();
            if force_string_content {
                let content = normalized
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::String(String::new()));
                normalized.insert(
                    "content".to_string(),
                    serde_json::Value::String(render_openai_message_content_as_text(&content)),
                );
            }
            serde_json::Value::Object(normalized)
        })
        .collect()
}

fn should_debug_message_shapes() -> bool {
    std::env::var("HARN_DEBUG_MESSAGE_SHAPES")
        .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub(crate) fn debug_log_message_shapes(label: &str, messages: &[serde_json::Value]) {
    if !should_debug_message_shapes() {
        return;
    }
    let summary = messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let role = message
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let content_kind = match message.get("content") {
                Some(serde_json::Value::String(_)) => "string",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Array(_)) => "array",
                Some(serde_json::Value::Object(_)) => "object",
                Some(_) => "other",
                None => "missing",
            };
            let has_tool_call_id = message.get("tool_call_id").is_some();
            let tool_calls = message
                .get("tool_calls")
                .and_then(|value| value.as_array())
                .map(|calls| calls.len())
                .unwrap_or(0);
            let has_reasoning = message
                .get("reasoning")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            format!(
                "#{idx}:{role}:content={content_kind}:tool_call_id={has_tool_call_id}:tool_calls={tool_calls}:reasoning={has_reasoning}"
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    crate::events::log_info("llm.message_shape", &format!("{label}: {summary}"));
}

/// Build a tagged, provider-prefixed error message from a non-2xx HTTP
/// response so downstream agent loops can react (e.g. trigger compaction on
/// `context_overflow`, back off on `rate_limited`, surface everything else as
/// `http_error`). Shared by both streaming and non-streaming transports so
/// the classification never drifts between them.
pub(crate) fn classify_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> String {
    // Patterns cover vLLM, OpenAI, Anthropic, and most OpenAI-compatible
    // servers. Lowercased once for cheap matching.
    let body_lower = body.to_lowercase();
    let is_context_overflow = body_lower.contains("maximum context length")
        || body_lower.contains("context length")
        || body_lower.contains("context_length_exceeded")
        || body_lower.contains("prompt is too long")
        || body_lower.contains("prompt_tokens_exceeded")
        || body_lower.contains("this model's maximum context")
        || body_lower.contains("exceeds the maximum")
        || (body_lower.contains("max_tokens") && body_lower.contains("exceed"));
    let tag = if is_context_overflow {
        "context_overflow"
    } else if status.as_u16() == 429 {
        "rate_limited"
    } else {
        "http_error"
    };
    let mut msg = format!("{provider} HTTP {status} [{tag}]: {body}");
    if let Some(ra) = retry_after {
        msg.push_str(&format!(" (retry-after: {ra})"));
    }
    msg
}

/// Dispatch an LLM API call to the appropriate provider. This is the main
/// entry point that routes to provider-specific implementations via the
/// provider plugin architecture.
///
/// The dispatch order is:
/// 1. Check the thread-local provider registry (populated by `register_default_providers`)
/// 2. Fall back to config-based resolution (for dynamically-configured providers)
/// 3. Use the legacy inline dispatch as a final fallback
async fn vm_call_llm_api(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;

    // Dispatch through the provider registry. The registry is populated at
    // VM startup by `register_default_providers()`. For providers not in the
    // registry (e.g. custom config-defined providers), we fall through to
    // config-based resolution which determines the API style and delegates
    // to the appropriate transport.
    if super::provider::is_provider_registered(provider) {
        return dispatch_to_registered_provider(opts, delta_tx).await;
    }

    // Fallback for providers not in the registry: resolve via config and
    // dispatch based on API style (anthropic vs openai-compatible vs ollama).
    let resolved = super::helpers::ResolvedProvider::resolve(provider);
    let is_ollama = provider == "ollama" || resolved.endpoint.contains("/api/chat");
    let is_anthropic = resolved.is_anthropic_style;

    let body = if is_ollama {
        super::providers::OllamaProvider::build_request_body(opts)
    } else if is_anthropic {
        super::providers::AnthropicProvider::build_request_body(opts)
    } else {
        super::providers::OpenAiCompatibleProvider::build_request_body(opts, false)
    };

    vm_call_llm_api_with_body(opts, delta_tx, body, is_anthropic, is_ollama).await
}

/// Dispatch to a registered provider by name.
///
/// Provider selection uses trait methods (`is_mock()`, `is_local()`,
/// `is_anthropic_style()`) instead of string comparisons so that each
/// provider owns its own dispatch semantics.
async fn dispatch_to_registered_provider(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    // Providers are zero-cost unit structs, so we construct them inline
    // rather than borrowing from the registry (which would conflict with
    // the RefCell across await points).
    let provider = &opts.provider;
    let resolved = super::helpers::ResolvedProvider::resolve(provider);

    // Build a concrete provider and dispatch via trait methods.
    let mock = super::providers::MockProvider;
    if mock.is_mock() && provider == mock.name() {
        return mock.chat_impl(opts, delta_tx).await;
    }

    let ollama = super::providers::OllamaProvider;
    if (provider == ollama.name() || resolved.endpoint.contains("/api/chat")) && ollama.is_local() {
        return ollama.chat_impl(opts, delta_tx).await;
    }

    if resolved.is_anthropic_style {
        let anthropic = super::providers::AnthropicProvider;
        return anthropic.chat_impl(opts, delta_tx).await;
    }

    // Default: OpenAI-compatible
    super::providers::OpenAiCompatibleProvider::new(provider.to_string())
        .chat_impl(opts, delta_tx)
        .await
}

/// Execute an LLM API call with a pre-built request body. This is the shared
/// transport layer used by all provider implementations. It handles:
/// - Provider-specific overrides merging
/// - Stream vs non-stream transport selection
/// - HTTP error classification
/// - SSE and NDJSON response parsing
///
/// Provider implementations call this after building their provider-specific
/// request body via `build_request_body()`.
pub(crate) async fn vm_call_llm_api_with_body(
    opts: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
    mut body: serde_json::Value,
    is_anthropic_style: bool,
    is_ollama: bool,
) -> Result<LlmResult, VmError> {
    let provider = &opts.provider;
    let model = &opts.model;
    let wants_streaming = delta_tx.is_some() && opts.stream;

    let resolved = super::helpers::ResolvedProvider::resolve(provider);
    let use_stream_transport = if is_ollama && !opts.stream {
        crate::events::log_warn(
            "llm",
            "stream=false is not supported by Ollama, using streaming",
        );
        true
    } else {
        wants_streaming || is_ollama
    };

    // Merge provider-specific overrides
    if let Some(ref overrides) = opts.provider_overrides {
        if let Some(obj) = overrides.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
    }

    if let Some(messages) = body.get("messages").and_then(|value| value.as_array()) {
        debug_log_message_shapes(
            &format!("outbound provider={provider} model={model}"),
            messages,
        );
    }

    if use_stream_transport {
        body["stream"] = serde_json::json!(true);
        // OpenAI-style: request usage in the final streaming chunk.
        if !is_anthropic_style && !is_ollama {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
    }

    // Reuse shared clients for connection pooling and TLS session caching.
    let client = if use_stream_transport {
        super::shared_streaming_client().clone()
    } else {
        super::shared_blocking_client().clone()
    };

    // Send request
    let req = client
        .post(resolved.url())
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(opts.resolve_timeout()))
        .json(&body);
    let req = resolved.apply_headers(req, &opts.api_key);

    if use_stream_transport {
        let tx = if let Some(tx) = delta_tx {
            tx
        } else {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            tx
        };
        let response = req.send().await.map_err(|e| {
            let kind = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else if e.is_request() {
                "request_build"
            } else if e.is_body() {
                "body"
            } else {
                "unknown"
            };
            // Include Debug repr for "unknown" errors to surface the inner cause
            let detail = if kind == "unknown" {
                format!("{provider} stream error ({kind}): {e:?}")
            } else {
                format!("{provider} stream error ({kind}): {e}")
            };
            VmError::Thrown(VmValue::String(Rc::from(detail)))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = response.text().await.unwrap_or_default();
            let msg = classify_http_error(provider, status, retry_after.as_deref(), &body);
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
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

    // Critical: check HTTP status BEFORE attempting to parse the body as an
    // LLM response. Previously this path went straight to `.json().await` and
    // silently garbled error responses — so a vLLM "prompt too long for
    // model" 400 came back as an empty/malformed parse result and the agent
    // loop kept retrying against the same oversized context.
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = response.text().await.unwrap_or_default();
        let msg = classify_http_error(provider, status, retry_after.as_deref(), &body);
        return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
    }

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
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = json["stop_reason"].as_str().map(|s| s.to_string());

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
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

        let message = &json["choices"][0]["message"];
        let (text, extracted_thinking) = normalize_openai_message_text(message);
        let mut blocks = if text.is_empty() {
            Vec::new()
        } else {
            vec![serde_json::json!({"type": "output_text", "text": text, "visibility": "public"})]
        };
        if !extracted_thinking.is_empty() {
            blocks.insert(
                0,
                serde_json::json!({
                    "type": "reasoning",
                    "text": extracted_thinking,
                    "visibility": "private",
                }),
            );
        }

        let mut tool_calls = Vec::new();
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: serde_json::Value = match serde_json::from_str(args_str) {
                    Ok(v) => v,
                    Err(e) => {
                        serde_json::json!({
                            "__parse_error": format!(
                                "Could not parse tool arguments as JSON: {}. Raw input: {}",
                                e,
                                &args_str[..args_str.len().min(200)]
                            )
                        })
                    }
                };
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
                    "arguments": arguments.clone(),
                    "visibility": "internal",
                }));
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = json["choices"][0]["finish_reason"]
            .as_str()
            .map(|s| s.to_string());

        if text.is_empty()
            && extracted_thinking.is_empty()
            && output_tokens > 0
            && tool_calls.is_empty()
        {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "openai-compatible model {model} reported completion_tokens={output_tokens} but delivered no content, reasoning, or tool calls"
            )))));
        }

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: if extracted_thinking.is_empty() {
                None
            } else {
                Some(extracted_thinking)
            },
            stop_reason,
            blocks,
        })
    }
}

/// Extract cache-read token count from a provider `usage` JSON value,
/// covering Anthropic, OpenAI (and OpenAI-compatibles), and OpenRouter
/// passthrough field shapes. Returns 0 when the provider doesn't report it.
fn extract_cache_read_tokens(usage: &serde_json::Value) -> i64 {
    // Anthropic / OpenRouter passthrough: usage.cache_read_input_tokens
    if let Some(n) = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenAI (and vLLM/SGLang when configured): usage.prompt_tokens_details.cached_tokens
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // Some OpenRouter responses nest under usage.cache_read_tokens or
    // usage.cached_prompt_tokens — be permissive.
    if let Some(n) = usage.get("cache_read_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    if let Some(n) = usage.get("cached_prompt_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    0
}

/// Extract cache-write (creation) token count from a provider `usage` JSON.
/// Currently only Anthropic reports this explicitly.
fn extract_cache_write_tokens(usage: &serde_json::Value) -> i64 {
    usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
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
    let mut cache_read_tokens: i64 = 0;
    let mut cache_write_tokens: i64 = 0;

    // OpenAI tool-call streaming state
    let mut oai_tool_map: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();
    // OpenAI-compatible inline `<think>...</think>` splitter (qwen3/qwen3.5
    // via vLLM's chat_template_kwargs.enable_thinking). Strips thinking
    // tokens out of the visible delta stream so downstream consumers (tool
    // call parser, progress UI) only see the real response.
    let mut oai_thinking_splitter = ThinkingStreamSplitter::new();

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
                    let usage = &json["message"]["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
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
                    let usage = &json["usage"];
                    let cr = extract_cache_read_tokens(usage);
                    if cr > 0 {
                        cache_read_tokens = cr;
                    }
                    let cw = extract_cache_write_tokens(usage);
                    if cw > 0 {
                        cache_write_tokens = cw;
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
                let visible = oai_thinking_splitter.push(content);
                if !visible.is_empty() {
                    text.push_str(&visible);
                    let _ = delta_tx.send(visible.clone());
                    blocks.push(serde_json::json!({"type": "output_text", "text": visible, "visibility": "public"}));
                }
            }
            let reasoning_delta =
                extract_openai_message_field_as_text(delta, &["reasoning", "reasoning_content"]);
            if !reasoning_delta.is_empty() {
                append_paragraph(&mut thinking_text, &reasoning_delta);
                blocks.push(serde_json::json!({"type": "reasoning", "text": reasoning_delta, "visibility": "private"}));
            }

            // Capture finish_reason — only on first occurrence. OpenRouter
            // can send duplicate finish_reason chunks (upstream bug
            // qwen-code#2402) which truncate in-progress tool calls.
            if stop_reason.is_none() {
                if let Some(fr) = choice["finish_reason"].as_str() {
                    stop_reason = Some(fr.to_string());
                }
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

            // Usage in final chunk (OpenAI-style)
            if let Some(usage) = json.get("usage") {
                if let Some(n) = usage["prompt_tokens"].as_i64() {
                    input_tokens = n;
                }
                if let Some(n) = usage["completion_tokens"].as_i64() {
                    output_tokens = n;
                }
                let cr = extract_cache_read_tokens(usage);
                if cr > 0 {
                    cache_read_tokens = cr;
                }
                let cw = extract_cache_write_tokens(usage);
                if cw > 0 {
                    cache_write_tokens = cw;
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

    // Flush any carried-over characters from the thinking splitter and merge
    // its accumulated thinking into the primary thinking_text (which is used
    // by both Anthropic and OpenAI-compatible response shapes).
    let final_visible = oai_thinking_splitter.flush();
    if !final_visible.is_empty() {
        text.push_str(&final_visible);
        let _ = delta_tx.send(final_visible.clone());
        blocks.push(serde_json::json!({"type": "output_text", "text": final_visible, "visibility": "public"}));
    }
    if !oai_thinking_splitter.thinking.is_empty() {
        append_paragraph(&mut thinking_text, &oai_thinking_splitter.thinking);
    }

    let _ = in_thinking_block; // suppress unused warning

    if text.is_empty() && !thinking_text.is_empty() {
        text = thinking_text.clone();
        blocks
            .push(serde_json::json!({"type": "output_text", "text": text, "visibility": "public"}));
    }
    if text.is_empty() && thinking_text.is_empty() && output_tokens > 0 && tool_calls.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "openai-compatible model {model} reported completion_tokens={output_tokens} but delivered no content, reasoning, or tool calls"
        )))));
    }

    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
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

// =============================================================================
// Provider context window discovery
// =============================================================================
//
// Many OpenAI-compatible servers (vLLM, text-generation-inference, LocalAI,
// llama.cpp server, etc.) expose the model's actual `max_model_len` via
// `GET /v1/models`. Harn can query that once and use it to adapt
// auto-compaction thresholds to the real window instead of assuming a
// hardcoded 80K. This prevents the "server silently truncates the prompt"
// failure mode where the agent loses older turns without knowing.

use std::collections::HashMap as StdHashMap;
use std::sync::{Mutex as StdMutex, OnceLock as StdOnceLock};

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
    // Anthropic Claude models (all current Claude 3+ models have 200K)
    if model.starts_with("claude-") {
        return Some(200_000);
    }
    // OpenAI models
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
    // Google Gemini models
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
    let client = super::shared_utility_client();
    let url = format!("{}/api/show", base_url.trim_end_matches('/'));
    let body = serde_json::json!({"name": model});
    // Ollama is typically local — use a tight per-request timeout so we fail
    // fast when it isn't running, while still reusing the shared connection pool.
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
    // Ollama returns model_info.context_length or parameters with num_ctx
    if let Some(n) = json
        .pointer("/model_info/general.context_length")
        .or_else(|| json.pointer("/model_info/context_length"))
        .and_then(|v| v.as_u64())
    {
        return Some(n as usize);
    }
    // Also check OLLAMA_NUM_CTX env override — user may have configured
    // a larger context window for this Ollama instance.
    if let Ok(val) = std::env::var("OLLAMA_NUM_CTX") {
        if let Ok(n) = val.parse::<usize>() {
            return Some(n);
        }
    }
    // Ollama's default context is model-dependent but commonly 2048-8192.
    // We return None to let the caller use its default threshold.
    None
}

/// Fetch context window from an OpenAI-compatible `/models` endpoint.
async fn fetch_openai_compatible_context_window(
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: &str,
) -> Option<usize> {
    let pdef = crate::llm_config::provider_config(provider);
    let client = super::shared_utility_client();
    let url = format!("{base_url}/models");
    let req = client
        .get(&url)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10));
    let req = apply_auth_headers(req, api_key, pdef);
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
    // 1. Check hardcoded known models first (Anthropic, OpenAI, Gemini).
    if let Some(n) = known_model_context_window(model) {
        return Some(n);
    }

    // 2. Ollama has its own model info endpoint.
    if provider == "ollama" {
        return fetch_ollama_context_window(model, base_url).await;
    }

    // 3. OpenAI-compatible providers: query /models endpoint.
    let is_openai_compatible = matches!(
        provider,
        "local"
            | "openai"
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

    // Tier-2 hard limit: derived from actual context window.
    if !user_specified_hard_limit {
        ac.hard_limit_tokens = Some(effective);
    } else if let Some(ref mut hl) = ac.hard_limit_tokens {
        // Clamp user's hard limit down if it would overflow.
        if *hl > effective {
            *hl = effective;
        }
    }

    // Tier-1: keep the configured lightweight threshold, but clamp down
    // if it exceeds the hard limit (which would make tier-1 pointless).
    if user_specified_threshold {
        if ac.token_threshold > effective {
            ac.token_threshold = effective;
        }
    } else {
        // Default tier-1 threshold: either the configured default (48K)
        // or 65% of max context, whichever is lower. This keeps the full
        // conversation visible until we're genuinely running low on headroom.
        let tier1_from_context = (max_ctx * 13) / 20; // 65%
        if ac.token_threshold > tier1_from_context {
            ac.token_threshold = tier1_from_context;
        }
    }
}

/// Split `<think>...</think>` blocks out of an OpenAI-compatible response
/// text. Returns `(visible_text, thinking_text)`. Handles multiple thinking
/// blocks, malformed/unclosed tags (best-effort), and preserves original
/// whitespace in the visible portion.
///
/// Used for Qwen3/Qwen3.5 thinking via vLLM's `chat_template_kwargs.enable_thinking`.
pub(crate) fn split_openai_thinking_blocks(raw: &str) -> (String, String) {
    if !raw.contains("<think>") {
        return (raw.to_string(), String::new());
    }
    let mut visible = String::new();
    let mut thinking = String::new();
    let mut rest = raw;
    loop {
        if let Some(start) = rest.find("<think>") {
            visible.push_str(&rest[..start]);
            let after_tag = &rest[start + "<think>".len()..];
            if let Some(end) = after_tag.find("</think>") {
                thinking.push_str(&after_tag[..end]);
                rest = &after_tag[end + "</think>".len()..];
            } else {
                // Unclosed <think> — treat everything after as thinking and stop.
                thinking.push_str(after_tag);
                break;
            }
        } else {
            visible.push_str(rest);
            break;
        }
    }
    // Trim a single leading newline from visible if we stripped a leading
    // thinking block — the model often emits `<think>...</think>\nActual`
    // and we don't want the blank line to linger.
    let visible = visible.trim_start_matches('\n').to_string();
    (visible, thinking.trim().to_string())
}

/// Incremental splitter for OpenAI-style streaming content that may contain
/// `<think>...</think>` blocks. Buffers a small suffix to handle tags split
/// across delta chunks. Only emits visible (non-thinking) content to the
/// delta channel; accumulates thinking separately for the final result.
#[derive(Default)]
pub(crate) struct ThinkingStreamSplitter {
    /// True while we're inside a `<think>` block.
    in_thinking: bool,
    /// Carryover characters from the last delta that might be the start of a
    /// `<think>` or `</think>` tag. Never longer than `</think>`.len() - 1.
    carry: String,
    /// Accumulated thinking text (returned at the end).
    pub thinking: String,
}

impl ThinkingStreamSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a new delta chunk. Returns the visible portion to forward
    /// downstream (may be empty if the entire chunk was part of a thinking
    /// block or got held back as carry).
    pub fn push(&mut self, delta: &str) -> String {
        let combined = {
            let mut s = std::mem::take(&mut self.carry);
            s.push_str(delta);
            s
        };
        let mut visible_out = String::new();
        let mut cursor = 0usize;
        let bytes = combined.as_bytes();
        while cursor < bytes.len() {
            if self.in_thinking {
                // Look for </think> in the remainder.
                if let Some(rel) = combined[cursor..].find("</think>") {
                    self.thinking.push_str(&combined[cursor..cursor + rel]);
                    cursor += rel + "</think>".len();
                    self.in_thinking = false;
                } else {
                    // Hold back up to len("</think>")-1 chars as potential
                    // split-tag carry; emit the rest into thinking.
                    let hold = "</think>".len() - 1;
                    let remaining = combined.len() - cursor;
                    if remaining <= hold {
                        self.carry.push_str(&combined[cursor..]);
                    } else {
                        let mut split = combined.len() - hold;
                        while split > cursor && !combined.is_char_boundary(split) {
                            split -= 1;
                        }
                        self.thinking.push_str(&combined[cursor..split]);
                        self.carry.push_str(&combined[split..]);
                    }
                    return visible_out;
                }
            } else {
                // Look for <think> in the remainder.
                if let Some(rel) = combined[cursor..].find("<think>") {
                    visible_out.push_str(&combined[cursor..cursor + rel]);
                    cursor += rel + "<think>".len();
                    self.in_thinking = true;
                } else {
                    // Hold back len("<think>")-1 chars as potential split-tag
                    // carry; emit the rest as visible.
                    let hold = "<think>".len() - 1;
                    let remaining = combined.len() - cursor;
                    if remaining <= hold {
                        self.carry.push_str(&combined[cursor..]);
                    } else {
                        let mut split = combined.len() - hold;
                        // Floor to the nearest char boundary so we never slice
                        // inside a multi-byte UTF-8 codepoint (e.g. em-dash).
                        while split > cursor && !combined.is_char_boundary(split) {
                            split -= 1;
                        }
                        visible_out.push_str(&combined[cursor..split]);
                        self.carry.push_str(&combined[split..]);
                    }
                    return visible_out;
                }
            }
        }
        visible_out
    }

    /// Flush any remaining carry as visible or thinking, depending on state.
    /// Called when the stream terminates.
    pub fn flush(&mut self) -> String {
        let rest = std::mem::take(&mut self.carry);
        if self.in_thinking {
            self.thinking.push_str(&rest);
            String::new()
        } else {
            rest
        }
    }
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

        // Extract text delta from message.content or message.thinking.
        // Ollama emits content and thinking as separate streaming channels
        // for models with reasoning capability (gemma3/gemma4, qwen3, etc.)
        // — we always set `think: true` in the request so the thinking
        // tokens are delivered rather than silently dropped. See the
        // `is_ollama` branch in `vm_call_llm_api` for that flag.
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

    // Guard against upstream parser bugs that report generated tokens but
    // deliver no visible content. Observed with `gemma4:26b` + ollama's
    // server-side `PARSER gemma4` on tool-heavy system prompts: the server
    // claims `eval_count` in the tens, but every streaming delta is empty
    // and the done chunk's `message.content`/`message.thinking` are both
    // empty strings. Silently returning an empty text turns the agent loop
    // into a no-op that burns iterations. Surface this as a hard error so
    // callers can decide whether to retry, switch models, or abort.
    if text.is_empty() && output_tokens > 0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama model {model} reported eval_count={output_tokens} but delivered no content or thinking — likely a server-side parser bug; try a different model"
        )))));
    }

    Ok(LlmResult {
        text,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: result_model,
        provider: "ollama".to_string(),
        thinking,
        stop_reason: None,
        blocks,
    })
}

pub(crate) fn ollama_num_ctx_override() -> Option<u64> {
    for key in [
        "BURIN_OLLAMA_NUM_CTX",
        "OLLAMA_CONTEXT_LENGTH",
        "OLLAMA_NUM_CTX",
    ] {
        if let Ok(raw) = std::env::var(key) {
            if let Ok(parsed) = raw.trim().parse::<u64>() {
                if parsed > 0 {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

pub(crate) fn ollama_keep_alive_override() -> Option<serde_json::Value> {
    for key in ["BURIN_OLLAMA_KEEP_ALIVE", "OLLAMA_KEEP_ALIVE"] {
        if let Ok(raw) = std::env::var(key) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(match trimmed.to_ascii_lowercase().as_str() {
                    "default" => serde_json::json!("30m"),
                    "forever" | "infinite" | "-1" => serde_json::json!(-1),
                    _ => {
                        if let Ok(n) = trimmed.parse::<i64>() {
                            serde_json::json!(n)
                        } else {
                            serde_json::json!(trimmed)
                        }
                    }
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        classify_http_error, normalize_openai_message_text, ollama_keep_alive_override,
        ollama_num_ctx_override, split_openai_thinking_blocks,
        vm_call_llm_full_streaming_offthread, LlmCallOptions, LlmRequestPayload,
        ThinkingStreamSplitter,
    };
    use crate::value::VmValue;
    use std::rc::Rc;

    #[test]
    fn thinking_split_no_tags_returns_original() {
        let (visible, thinking) = split_openai_thinking_blocks("just a plain response");
        assert_eq!(visible, "just a plain response");
        assert_eq!(thinking, "");
    }

    #[test]
    fn thinking_split_single_block() {
        let raw = "<think>step by step reasoning</think>\nThe answer is 42.";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "The answer is 42.");
        assert_eq!(thinking, "step by step reasoning");
    }

    #[test]
    fn thinking_split_multiple_blocks() {
        let raw = "<think>first</think>hello <think>second</think>world";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "hello world");
        assert_eq!(
            thinking,
            "first\nsecond".replace('\n', "") /* joined with empty */
        );
        // Tolerate either concatenation strategy: important invariant is
        // neither block text leaked into visible.
        assert!(!visible.contains("first"));
        assert!(!visible.contains("second"));
    }

    #[test]
    fn thinking_split_unclosed_block_captures_remainder() {
        let raw = "<think>reasoning with no closing tag and then text";
        let (visible, thinking) = split_openai_thinking_blocks(raw);
        assert_eq!(visible, "");
        assert!(thinking.contains("reasoning with no closing tag"));
    }

    #[test]
    fn thinking_stream_splitter_handles_clean_boundaries() {
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("<think>");
        let v2 = s.push("reasoning");
        let v3 = s.push("</think>");
        let v4 = s.push("visible answer");
        let tail = s.flush();
        assert_eq!(v1, "");
        assert_eq!(v2, "");
        assert_eq!(v3, "");
        // Visible answer may be partially held as carry — concatenate
        // everything plus flush to get the full output.
        let combined = format!("{}{}{}{}{}", v1, v2, v3, v4, tail);
        assert_eq!(combined, "visible answer");
        assert_eq!(s.thinking, "reasoning");
    }

    #[test]
    fn thinking_stream_splitter_handles_split_tags() {
        // `<think>` split across deltas: `<thi` + `nk>rest`
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("<thi");
        let v2 = s.push("nk>inside</thi");
        let v3 = s.push("nk>after");
        let tail = s.flush();
        let combined = format!("{}{}{}{}", v1, v2, v3, tail);
        assert_eq!(combined, "after");
        assert_eq!(s.thinking, "inside");
    }

    #[test]
    fn thinking_stream_splitter_passthrough_without_tags() {
        let mut s = ThinkingStreamSplitter::new();
        let v1 = s.push("hello ");
        let v2 = s.push("world");
        let tail = s.flush();
        let combined = format!("{}{}{}", v1, v2, tail);
        assert_eq!(combined, "hello world");
        assert_eq!(s.thinking, "");
    }

    #[test]
    fn normalize_openai_message_text_uses_reasoning_when_content_missing() {
        let message = serde_json::json!({
            "reasoning": "hello from reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message);
        assert_eq!(visible, "hello from reasoning");
        assert_eq!(thinking, "hello from reasoning");
    }

    #[test]
    fn normalize_openai_message_text_merges_reasoning_and_inline_think_blocks() {
        let message = serde_json::json!({
            "content": "<think>inline reasoning</think>visible answer",
            "reasoning": "separate reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message);
        assert_eq!(visible, "visible answer");
        assert_eq!(thinking, "separate reasoning\ninline reasoning");
    }

    use crate::llm::env_lock;

    fn base_opts(provider: &str) -> LlmCallOptions {
        LlmCallOptions {
            provider: provider.to_string(),
            model: "test-model".to_string(),
            api_key: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            system: None,
            transcript_id: Some("tx-1".to_string()),
            transcript_summary: Some("summary".to_string()),
            transcript_metadata: Some(serde_json::json!({"scope": "test"})),
            max_tokens: 64,
            temperature: Some(0.2),
            top_p: Some(0.8),
            top_k: Some(40),
            stop: Some(vec!["STOP".to_string()]),
            seed: Some(7),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            response_format: Some("json".to_string()),
            json_schema: Some(serde_json::json!({"type": "object"})),
            output_schema: Some(serde_json::json!({"type": "object"})),
            output_validation: Some("error".to_string()),
            thinking: None,
            tools: Some(VmValue::String(Rc::from("vm-local-tools"))),
            native_tools: Some(vec![
                serde_json::json!({"type": "function", "function": {"name": "tool"}}),
            ]),
            tool_choice: Some(serde_json::json!({
                "type": "function",
                "function": {"name": "tool"}
            })),
            cache: true,
            stream: true,
            timeout: Some(5),
            idle_timeout: None,
            provider_overrides: Some(serde_json::json!({"custom_flag": true})),
        }
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn request_payload_is_send_safe_and_drops_vm_local_fields() {
        let payload = LlmRequestPayload::from(&base_opts("openai"));
        assert_send::<LlmRequestPayload>();
        assert_eq!(payload.provider, "openai");
        assert_eq!(payload.model, "test-model");
        assert!(payload.native_tools.is_some());
        assert!(payload.tool_choice.is_some());
        assert_eq!(
            payload.provider_overrides,
            Some(serde_json::json!({"custom_flag": true}))
        );
    }

    /// Accept a single connection with a bounded deadline so a buggy client
    /// can't wedge the test runner. Used by all localhost stubs in this
    /// module. Historical note: blocking `listener.accept()` has taken down
    /// the test suite at least twice.
    fn accept_with_deadline(listener: &std::net::TcpListener, label: &str) -> std::net::TcpStream {
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("restore blocking mode");
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                        .ok();
                    stream
                        .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                        .ok();
                    return stream;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        panic!("{label}: no client within 3s");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("{label}: accept failed: {e}"),
            }
        }
    }

    fn spawn_ollama_stub() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ollama stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            let mut stream = accept_with_deadline(&listener, "ollama stub");
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1\r\n"));

            let body = concat!(
                "{\"message\":{\"role\":\"assistant\",\"content\":\"hello \"},\"done\":false,\"model\":\"stub-model\"}\n",
                "{\"message\":{\"role\":\"assistant\",\"content\":\"world\"},\"done\":false}\n",
                "{\"done\":true,\"prompt_eval_count\":3,\"eval_count\":2,\"model\":\"stub-model\"}\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (addr, handle)
    }

    fn spawn_ollama_stub_with_body_capture(
        captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ollama stub");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            let mut stream = accept_with_deadline(&listener, "ollama stub (capture)");
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or_default()
                .to_string();
            *captured.lock().expect("capture body") = Some(body);

            let body = concat!(
                "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":false}\n",
                "{\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        (addr, handle)
    }

    #[test]
    fn ollama_num_ctx_override_prefers_burin_env() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("BURIN_OLLAMA_NUM_CTX", "131072");
            std::env::remove_var("OLLAMA_CONTEXT_LENGTH");
            std::env::remove_var("OLLAMA_NUM_CTX");
        }
        assert_eq!(ollama_num_ctx_override(), Some(131072));
        unsafe {
            std::env::remove_var("BURIN_OLLAMA_NUM_CTX");
        }
    }

    #[test]
    fn ollama_keep_alive_override_normalizes_forever() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("BURIN_OLLAMA_KEEP_ALIVE", "forever");
            std::env::remove_var("OLLAMA_KEEP_ALIVE");
        }
        assert_eq!(ollama_keep_alive_override(), Some(serde_json::json!(-1)));
        unsafe {
            std::env::remove_var("BURIN_OLLAMA_KEEP_ALIVE");
        }
    }

    #[test]
    fn offthread_streaming_completes_inside_localset() {
        let _guard = env_lock().lock().expect("env lock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let (addr, server) = spawn_ollama_stub();
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        vm_call_llm_full_streaming_offthread(&opts, tx),
                    )
                    .await
                    .expect("llm call timed out")
                    .expect("llm call should succeed");

                    let mut deltas = Vec::new();
                    while let Ok(delta) = rx.try_recv() {
                        deltas.push(delta);
                    }
                    (result, deltas)
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe {
                    std::env::set_var("OLLAMA_HOST", value);
                },
                None => unsafe {
                    std::env::remove_var("OLLAMA_HOST");
                },
            }

            server.join().expect("stub server");

            let (result, deltas) = result;
            assert_eq!(result.text, "hello world");
            assert_eq!(result.model, "stub-model");
            assert_eq!(result.input_tokens, 3);
            assert_eq!(result.output_tokens, 2);
            assert_eq!(deltas.join(""), "hello world");
        });
    }

    #[test]
    fn ollama_chat_applies_env_runtime_overrides() {
        let _guard = env_lock().lock().expect("env lock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
            let (addr, server) = spawn_ollama_stub_with_body_capture(captured.clone());
            let prev_ollama_host = std::env::var("OLLAMA_HOST").ok();
            let prev_num_ctx = std::env::var("BURIN_OLLAMA_NUM_CTX").ok();
            let prev_keep_alive = std::env::var("BURIN_OLLAMA_KEEP_ALIVE").ok();
            unsafe {
                std::env::set_var("OLLAMA_HOST", format!("http://{addr}"));
                std::env::set_var("BURIN_OLLAMA_NUM_CTX", "131072");
                std::env::set_var("BURIN_OLLAMA_KEEP_ALIVE", "forever");
            }

            let local = tokio::task::LocalSet::new();
            let result = local
                .run_until(async {
                    let opts = base_opts("ollama");
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    vm_call_llm_full_streaming_offthread(&opts, tx)
                        .await
                        .expect("llm call should succeed")
                })
                .await;

            match prev_ollama_host {
                Some(value) => unsafe { std::env::set_var("OLLAMA_HOST", value) },
                None => unsafe { std::env::remove_var("OLLAMA_HOST") },
            }
            match prev_num_ctx {
                Some(value) => unsafe { std::env::set_var("BURIN_OLLAMA_NUM_CTX", value) },
                None => unsafe { std::env::remove_var("BURIN_OLLAMA_NUM_CTX") },
            }
            match prev_keep_alive {
                Some(value) => unsafe { std::env::set_var("BURIN_OLLAMA_KEEP_ALIVE", value) },
                None => unsafe { std::env::remove_var("BURIN_OLLAMA_KEEP_ALIVE") },
            }

            server.join().expect("stub server");
            assert_eq!(result.text, "ok");
            let body = captured
                .lock()
                .expect("captured body")
                .clone()
                .expect("request body");
            let json: serde_json::Value = serde_json::from_str(&body).expect("valid request json");
            assert_eq!(json["keep_alive"].as_i64(), Some(-1));
            assert_eq!(json["options"]["num_ctx"].as_u64(), Some(131072));
        });
    }

    // ---- HTTP error classification (v0.5.36 regression coverage) ----

    #[test]
    fn classify_tags_vllm_prompt_too_long_as_context_overflow() {
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            r#"{"object":"error","message":"This model's maximum context length is 8192 tokens. However, your prompt is too long (10234 tokens)."}"#,
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
        assert!(msg.starts_with("local HTTP 400 Bad Request"));
        assert!(!msg.contains("(retry-after"));
    }

    #[test]
    fn classify_tags_openai_context_length_exceeded_as_context_overflow() {
        let msg = classify_http_error(
            "openai",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
    }

    #[test]
    fn classify_tags_429_with_retry_after_as_rate_limited() {
        let msg = classify_http_error(
            "anthropic",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("12"),
            r#"{"error":{"type":"rate_limit_error","message":"quota exceeded"}}"#,
        );
        assert!(msg.contains("[rate_limited]"), "msg was: {msg}");
        assert!(msg.ends_with("(retry-after: 12)"), "msg was: {msg}");
    }

    #[test]
    fn classify_tags_opaque_500_as_http_error() {
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            None,
            "upstream exploded",
        );
        assert!(msg.contains("[http_error]"), "msg was: {msg}");
        assert!(msg.contains("upstream exploded"));
    }

    #[test]
    fn classify_429_with_context_body_still_prefers_context_overflow() {
        // A provider that returns 429 for context-overflow (seen with some
        // OpenAI-compat servers) should classify by body, not by status,
        // because the caller's reaction differs (compact vs. back off).
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("1"),
            "prompt is too long",
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
    }

    /// Bind a stub listener + spawn a responder that serves a single canned
    /// HTTP error response, then returns its join handle. The listener uses
    /// a bounded accept so a misrouted client can never hang the test
    /// process — any failure to connect within 3s causes the thread to
    /// exit, unblocking `join()`.
    fn spawn_openai_error_stub(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind openai stub");
        let addr = listener.local_addr().expect("stub addr");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let handle = std::thread::spawn(move || {
            // Accept window intentionally generous (was 3s and proved flaky
            // under parallel-test load on CI / cold macOS workers — the
            // listener would exit before reqwest established the connection
            // so the client saw a refused connect and the assertion fired
            // against a transport error instead of the HTTP 500 payload).
            // 15s matches the `.config/nextest.toml` slow-test threshold
            // and still falls well inside the 60s hard termination cap.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            };
            // Once we have a client, use a bounded read/write so a stuck
            // client can't wedge the suite either.
            stream
                .set_nonblocking(false)
                .expect("restore blocking mode on accepted stream");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            let mut buf = vec![0u8; 16384];
            let _ = stream.read(&mut buf);
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra_headers}connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (addr, handle)
    }

    /// Single-entrypoint helper that serializes env-var mutation and the
    /// LLM call behind `env_lock`, so parallel streaming error tests can't
    /// clobber each other's `LOCAL_LLM_BASE_URL` and leak an unconnected
    /// stub whose `join()` would hang the test binary.
    fn run_streaming_error_case(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> String {
        let _guard = env_lock().lock().expect("env lock");
        let (addr, server) = spawn_openai_error_stub(status_line, extra_headers, body);
        let prev = std::env::var("LOCAL_LLM_BASE_URL").ok();
        unsafe {
            std::env::set_var("LOCAL_LLM_BASE_URL", format!("http://{addr}"));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("runtime");
        let err = runtime.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut opts = base_opts("local");
                    // Drop tools/schemas so the request body stays minimal.
                    opts.tools = None;
                    opts.native_tools = None;
                    opts.tool_choice = None;
                    opts.response_format = None;
                    opts.json_schema = None;
                    opts.output_schema = None;
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    let call = tokio::time::timeout(
                        // Must stay strictly less than the stub's 15s accept
                        // window so we always fail with the actual HTTP 500
                        // classification instead of a generic timeout.
                        std::time::Duration::from_secs(12),
                        vm_call_llm_full_streaming_offthread(&opts, tx),
                    )
                    .await;
                    match call {
                        Ok(Ok(_)) => panic!("expected streaming call to fail"),
                        Ok(Err(err)) => err.to_string(),
                        Err(_) => panic!("streaming call timed out"),
                    }
                })
                .await
        });
        match prev {
            Some(v) => unsafe { std::env::set_var("LOCAL_LLM_BASE_URL", v) },
            None => unsafe { std::env::remove_var("LOCAL_LLM_BASE_URL") },
        }
        // Bounded join: the stub's internal deadline guarantees this returns.
        let _ = server.join();
        err
    }

    #[test]
    fn streaming_path_classifies_context_overflow() {
        let err = run_streaming_error_case(
            "HTTP/1.1 400 Bad Request",
            "",
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your prompt is too long."}}"#,
        );
        assert!(err.contains("[context_overflow]"), "err was: {err}");
        assert!(err.contains("local HTTP 400"), "err was: {err}");
    }

    #[test]
    fn streaming_path_classifies_rate_limit_with_retry_after() {
        let err = run_streaming_error_case(
            "HTTP/1.1 429 Too Many Requests",
            "retry-after: 7\r\n",
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(err.contains("[rate_limited]"), "err was: {err}");
        assert!(err.contains("(retry-after: 7)"), "err was: {err}");
    }

    #[test]
    fn streaming_path_classifies_opaque_500_as_http_error() {
        let err = run_streaming_error_case(
            "HTTP/1.1 500 Internal Server Error",
            "",
            r#"{"error":"upstream exploded"}"#,
        );
        assert!(err.contains("[http_error]"), "err was: {err}");
        assert!(err.contains("upstream exploded"), "err was: {err}");
    }
}
