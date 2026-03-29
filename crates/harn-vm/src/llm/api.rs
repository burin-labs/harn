use std::collections::BTreeMap;
use std::rc::Rc;

use crate::llm_config;
use crate::value::{VmError, VmValue};

use super::helpers::vm_value_dict_to_json;
use super::mock::{
    fixture_hash, get_replay_mode, load_fixture, mock_llm_response, save_fixture, LlmReplayMode,
};

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

    let result = vm_call_llm_api(
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
    )
    .await;

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
) -> Result<LlmResult, VmError> {
    let llm_timeout = std::env::var("HARN_LLM_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(llm_timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // Resolve provider config for base URL and auth
    let pdef = llm_config::provider_config(provider);
    let is_anthropic_style = pdef
        .map(|p| p.chat_endpoint.contains("/messages"))
        .unwrap_or(provider == "anthropic");

    if is_anthropic_style {
        // Anthropic-style API (system as top-level field, content blocks response)
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/messages");

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

        // Native tool use for Anthropic
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);

        // Apply auth from config
        req = apply_auth_headers(req, api_key, pdef);

        // Apply extra headers from config
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
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

        // Extract text and tool_use blocks from Anthropic response
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
        // OpenAI-compatible API (system as message, choices response)
        let base_url = pdef
            .map(llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let endpoint = pdef
            .map(|p| p.chat_endpoint.as_str())
            .unwrap_or("/chat/completions");

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

        // Structured output for OpenAI
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

        // Native tool use
        if let Some(tools) = native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }

        let mut req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .json(&body);

        // Apply auth from config
        req = apply_auth_headers(req, api_key, pdef);

        // Apply extra headers from config
        if let Some(p) = pdef {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
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

        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{provider} API error: {err}"
            )))));
        }

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // Extract tool calls from OpenAI format
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

/// Apply auth headers to a request based on provider config.
pub(crate) fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
    pdef: Option<&llm_config::ProviderDef>,
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
