use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::helpers::vm_value_dict_to_json;
use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, save_fixture, LlmReplayMode,
};

/// Sender for streaming text deltas from an in-flight LLM call.
pub(crate) type DeltaSender = tokio::sync::mpsc::UnboundedSender<String>;

// =============================================================================
// LLM response type
// =============================================================================

pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model: String,
}

pub(crate) fn vm_build_llm_result(result: &LlmResult, parsed_json: Option<VmValue>) -> VmValue {
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

    VmValue::Dict(Rc::new(dict))
}

// =============================================================================
// Core LLM call with all options
// =============================================================================

#[allow(clippy::too_many_arguments)]
pub(crate) async fn vm_call_llm_full(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(
        provider,
        model,
        api_key,
        messages,
        system,
        max_tokens,
        response_format,
        json_schema,
        temperature,
        native_tools,
        None,
    )
    .await
}

/// Like [`vm_call_llm_full`] but streams text deltas to `delta_tx` as they
/// arrive from the LLM provider via SSE.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn vm_call_llm_full_streaming(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    vm_call_llm_full_inner(
        provider,
        model,
        api_key,
        messages,
        system,
        max_tokens,
        response_format,
        json_schema,
        temperature,
        native_tools,
        Some(delta_tx),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn vm_call_llm_full_inner(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    // Mock provider: return deterministic response without API call.
    if provider == "mock" {
        return Ok(mock_llm_response(messages, system, native_tools));
    }

    let replay_mode = get_replay_mode();
    let hash = fixture_hash(model, messages, system);

    // In replay mode, return cached fixture
    if replay_mode == LlmReplayMode::Replay {
        if let Some(result) = load_fixture(&hash) {
            return Ok(result);
        }
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "No fixture found for LLM call (hash: {hash}). Run with --record first."
        )))));
    }

    let call_api = |dtx: Option<DeltaSender>| {
        vm_call_llm_api(
            provider,
            model,
            api_key,
            messages,
            system,
            max_tokens,
            response_format,
            json_schema,
            temperature,
            native_tools,
            dtx,
        )
    };

    let result = call_api(delta_tx).await;

    // On failure, check for provider fallback chain
    let result = match result {
        Ok(r) => r,
        Err(primary_err) => {
            if let Some(pdef) = crate::llm_config::provider_config(provider) {
                if let Some(ref fallback_provider) = pdef.fallback {
                    // Note: uses the same model name. Users should configure
                    // compatible model names or use providers.toml aliases.
                    // Resolve fallback provider's API key
                    let fb_key =
                        super::helpers::vm_resolve_api_key(fallback_provider).unwrap_or_default();
                    if !fb_key.is_empty() {
                        let fb_result = vm_call_llm_api(
                            fallback_provider,
                            model,
                            &fb_key,
                            messages,
                            system,
                            max_tokens,
                            response_format,
                            json_schema,
                            temperature,
                            native_tools,
                            None, // no streaming for fallback
                        )
                        .await;
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

#[allow(clippy::too_many_arguments)]
async fn vm_call_llm_api(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
    response_format: Option<&str>,
    json_schema: Option<&BTreeMap<String, VmValue>>,
    temperature: Option<f64>,
    native_tools: Option<&[serde_json::Value]>,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let streaming = delta_tx.is_some();
    let llm_timeout = std::env::var("HARN_LLM_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resolved = super::helpers::ResolvedProvider::resolve(provider);

    // Build request body based on API style
    let mut body = if resolved.is_anthropic_style {
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
        });
        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        body
    } else {
        let mut msgs = Vec::new();
        if let Some(sys) = system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(messages.iter().cloned());

        let mut body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
        });
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if response_format == Some("json") {
            if let Some(schema) = json_schema {
                let schema_json = vm_value_dict_to_json(schema);
                body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema_json,
                        "strict": true,
                    }
                });
            } else {
                body["response_format"] = serde_json::json!({"type": "json_object"});
            }
        }
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        body
    };

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
    let req = resolved.apply_headers(req, api_key);

    if streaming {
        return vm_call_llm_api_sse(req, provider, model, &resolved, delta_tx.unwrap()).await;
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
        let mut tool_calls = Vec::new();

        if let Some(content) = json["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text.push_str(t);
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

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
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
            }
        }

        let input_tokens = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let output_tokens = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);

        Ok(LlmResult {
            text,
            tool_calls,
            input_tokens,
            output_tokens,
            model: model.to_string(),
        })
    }
}

/// Consume an SSE streaming response, forwarding text deltas to `delta_tx`
/// while accumulating the full result.
async fn vm_call_llm_api_sse(
    request: reqwest::RequestBuilder,
    provider: &str,
    model: &str,
    resolved: &super::helpers::ResolvedProvider<'_>,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use reqwest_eventsource::{Event, EventSource};
    use tokio_stream::StreamExt;

    let mut es = EventSource::new(request).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "{provider} stream setup error: {e}"
        ))))
    })?;

    let mut text = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;

    // Anthropic tool-use streaming state
    struct ToolBlock {
        id: String,
        name: String,
        input_json: String,
    }
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut current_tool: Option<ToolBlock> = None;

    // OpenAI tool-call streaming state: index -> (id, name, accumulated args)
    let mut oai_tool_map: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();

    while let Some(event) = es.next().await {
        match event {
            Ok(Event::Message(msg)) => {
                if msg.data == "[DONE]" {
                    break;
                }
                let json: serde_json::Value = match serde_json::from_str(&msg.data) {
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
                            if block["type"].as_str() == Some("tool_use") {
                                current_tool = Some(ToolBlock {
                                    id: block["id"].as_str().unwrap_or("").to_string(),
                                    name: block["name"].as_str().unwrap_or("").to_string(),
                                    input_json: String::new(),
                                });
                            }
                        }
                        Some("content_block_delta") => {
                            let delta = &json["delta"];
                            match delta["type"].as_str() {
                                Some("text_delta") => {
                                    if let Some(t) = delta["text"].as_str() {
                                        text.push_str(t);
                                        let _ = delta_tx.send(t.to_string());
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
                                let input: serde_json::Value =
                                    serde_json::from_str(&tool.input_json)
                                        .unwrap_or(serde_json::json!({}));
                                tool_calls.push(serde_json::json!({
                                    "id": tool.id,
                                    "name": tool.name,
                                    "arguments": input,
                                }));
                            }
                        }
                        Some("message_delta") => {
                            if let Some(n) = json["usage"]["output_tokens"].as_i64() {
                                output_tokens = n;
                            }
                        }
                        _ => {}
                    }
                } else {
                    // OpenAI-style SSE
                    if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                        text.push_str(content);
                        let _ = delta_tx.send(content.to_string());
                    }
                    // Accumulate tool call fragments
                    if let Some(calls) = json["choices"][0]["delta"]["tool_calls"].as_array() {
                        for call in calls {
                            let idx = call["index"].as_u64().unwrap_or(0);
                            let entry = oai_tool_map.entry(idx).or_insert_with(|| {
                                (
                                    call["id"].as_str().unwrap_or("").to_string(),
                                    call["function"]["name"].as_str().unwrap_or("").to_string(),
                                    String::new(),
                                )
                            });
                            if let Some(args) = call["function"]["arguments"].as_str() {
                                entry.2.push_str(args);
                            }
                        }
                    }
                    // Usage in final chunk (stream_options.include_usage)
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
            Ok(Event::Open) => {}
            Err(_) => break,
        }
    }

    es.close();

    // Finalize OpenAI tool calls
    if !oai_tool_map.is_empty() {
        let mut indices: Vec<u64> = oai_tool_map.keys().copied().collect();
        indices.sort_unstable();
        for idx in indices {
            let (id, name, args_str) = oai_tool_map.remove(&idx).unwrap();
            let arguments: serde_json::Value =
                serde_json::from_str(&args_str).unwrap_or(serde_json::json!({}));
            tool_calls.push(serde_json::json!({
                "id": id,
                "name": name,
                "arguments": arguments,
            }));
        }
    }

    Ok(LlmResult {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        model: model.to_string(),
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
