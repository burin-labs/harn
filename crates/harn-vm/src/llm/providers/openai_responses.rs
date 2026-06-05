//! Native OpenAI Responses API provider path.
//!
//! This is intentionally separate from the OpenAI-compatible chat-completions
//! adapter. Responses has first-class hosted tools, provider-managed
//! conversation state, and different output items, so keeping the wire builder
//! distinct prevents compatibility shims from leaking into generic
//! OpenAI-compatible providers.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, OutputFormat, ThinkingConfig};
use crate::value::{VmError, VmValue};

const RESPONSES_ENDPOINT: &str = "/responses";
const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";

pub(crate) struct OpenAiResponsesProvider;

impl OpenAiResponsesProvider {
    pub(crate) async fn call(
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        if request.provider != "openai" {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "api_mode: \"responses\" is only supported by provider \"openai\"; got provider \"{}\"",
                request.provider
            )))));
        }

        let clock = harn_clock::RealClock::arc();
        let started_ms = clock.monotonic_ms();
        let compact = request.compact.unwrap_or(false);
        let mut body = if compact {
            Self::build_compact_request_body(request)
        } else {
            Self::build_request_body(request)
        };
        if let Some(ref overrides) = request.provider_overrides {
            if let Some(object) = overrides.as_object() {
                for (key, value) in object {
                    body[key] = value.clone();
                }
            }
        }

        let mut resolved = crate::llm::helpers::ResolvedProvider::resolve(&request.provider);
        resolved.endpoint = if compact {
            RESPONSES_COMPACT_ENDPOINT
        } else {
            RESPONSES_ENDPOINT
        }
        .to_string();
        let client = crate::llm::shared_blocking_client().clone();
        let req = client
            .post(resolved.url())
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = resolved.apply_headers(req, &request.api_key);
        let response = req.send().await.map_err(|error| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "openai Responses API error: {error}"
            ))))
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = response.text().await.unwrap_or_default();
            let msg = crate::llm::providers::OpenAiCompatibleProvider::classify_http_error(
                "openai",
                status,
                retry_after.as_deref(),
                &body,
            )
            .message;
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(msg))));
        }

        let json: serde_json::Value = response.json().await.map_err(|error| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "openai Responses API response parse error: {error}"
            ))))
        })?;

        let mut result =
            crate::llm::api::parse_openai_responses_response(&json, "openai", &request.model)?;
        if result.telemetry.client_wall_ms.is_none() {
            result.telemetry.client_wall_ms = Some(elapsed_ms(&*clock, started_ms));
        }
        if result.telemetry.source.is_empty() {
            result.telemetry.source = crate::llm::api::telemetry_source::UNKNOWN.to_string();
        }
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        Ok(result)
    }

    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "input": responses_input_items(opts),
        });

        if let Some(ref system) = opts.system {
            body["instructions"] = serde_json::json!(system);
        }
        if opts.max_tokens > 0 {
            body["max_output_tokens"] = serde_json::json!(opts.max_tokens);
        }
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
        if let Some(ref previous_response_id) = opts.previous_response_id {
            body["previous_response_id"] = serde_json::json!(previous_response_id);
        }
        if let Some(store) = opts.store {
            body["store"] = serde_json::json!(store);
        }
        if let Some(background) = opts.background {
            body["background"] = serde_json::json!(background);
        }
        if let Some(ref truncation) = opts.truncation {
            body["truncation"] = serde_json::json!(truncation);
        }
        if let Some(ref include) = opts.include {
            body["include"] = serde_json::json!(include);
        }
        if let Some(max_tool_calls) = opts.max_tool_calls {
            body["max_tool_calls"] = serde_json::json!(max_tool_calls);
        }
        if let Some(ref tool_choice) = opts.tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }
        if let Some(reasoning) = responses_reasoning_config(&opts.thinking) {
            body["reasoning"] = reasoning;
        }
        match &opts.output_format {
            OutputFormat::Text => {}
            OutputFormat::JsonObject => {
                body["text"] = serde_json::json!({
                    "format": {"type": "json_object"}
                });
            }
            OutputFormat::JsonSchema { schema, strict } => {
                body["text"] = serde_json::json!({
                    "format": {
                        "type": "json_schema",
                        "name": "response",
                        "schema": schema,
                        "strict": strict,
                    }
                });
            }
        }

        let tools = responses_tools(opts);
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }

        crate::llm::fast_mode::apply_request_knob(&mut body, &opts.model, opts.fast);
        body
    }

    pub(crate) fn build_compact_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let wire_model = crate::llm_config::wire_model_id(&opts.model);
        let mut body = serde_json::json!({
            "model": wire_model,
            "input": responses_input_items(opts),
        });
        if let Some(ref system) = opts.system {
            body["instructions"] = serde_json::json!(system);
        }
        if let Some(ref previous_response_id) = opts.previous_response_id {
            body["previous_response_id"] = serde_json::json!(previous_response_id);
        }
        body
    }
}

fn responses_reasoning_config(thinking: &ThinkingConfig) -> Option<serde_json::Value> {
    match thinking {
        ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None,
        } => None,
        ThinkingConfig::Effort { level } => Some(serde_json::json!({
            "effort": level.as_str(),
        })),
        _ => None,
    }
}

fn responses_tools(opts: &LlmRequestPayload) -> Vec<serde_json::Value> {
    let mut tools = opts.provider_tools.clone();
    if let Some(native_tools) = opts.native_tools.as_ref() {
        tools.extend(native_tools.iter().map(responses_function_tool));
    }
    tools
}

fn responses_function_tool(tool: &serde_json::Value) -> serde_json::Value {
    if tool.get("type").and_then(serde_json::Value::as_str) != Some("function") {
        return tool.clone();
    }
    let Some(function) = tool.get("function").and_then(serde_json::Value::as_object) else {
        return tool.clone();
    };

    let mut out = serde_json::Map::new();
    out.insert("type".to_string(), serde_json::json!("function"));
    for key in ["name", "description", "parameters", "strict"] {
        if let Some(value) = function.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    for key in ["defer_loading", "namespace", "namespaces"] {
        if let Some(value) = tool.get(key).or_else(|| function.get(key)) {
            out.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

fn responses_input_items(opts: &LlmRequestPayload) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for message in &opts.messages {
        append_responses_message_items(message, &mut items);
    }
    if let Some(ref prefill) = opts.prefill {
        items.push(serde_json::json!({
            "role": "assistant",
            "content": prefill,
        }));
    }
    items
}

fn append_responses_message_items(message: &serde_json::Value, items: &mut Vec<serde_json::Value>) {
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("user");
    if role == "tool" {
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("call_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let output = message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                message
                    .get("content")
                    .map(serde_json::Value::to_string)
                    .unwrap_or_default()
            });
        items.push(serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        }));
        return;
    }

    if let Some(content) = message.get("content") {
        items.push(serde_json::json!({
            "role": role,
            "content": responses_message_content(role, content),
        }));
    }

    if let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(tool_call);
            let call_id = tool_call
                .get("id")
                .or_else(|| tool_call.get("call_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let name = function
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            items.push(serde_json::json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments_to_string(arguments),
            }));
        }
    }
}

fn responses_message_content(role: &str, content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .filter_map(|item| responses_content_item(role, item))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn responses_content_item(role: &str, item: &serde_json::Value) -> Option<serde_json::Value> {
    let item_type = item.get("type").and_then(serde_json::Value::as_str)?;
    match item_type {
        "text" | "input_text" | "output_text" => {
            let text = item
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let response_type = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            Some(serde_json::json!({"type": response_type, "text": text}))
        }
        "image" | "input_image" => {
            let image_url = item
                .get("url")
                .or_else(|| item.get("image_url"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let base64 = item.get("base64").and_then(serde_json::Value::as_str)?;
                    let media_type = item
                        .get("media_type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("image/png");
                    Some(format!("data:{media_type};base64,{base64}"))
                })?;
            Some(serde_json::json!({"type": "input_image", "image_url": image_url}))
        }
        "file" | "input_file" | "pdf" => {
            if let Some(file_id) = item
                .get("file_id")
                .or_else(|| item.get("file"))
                .and_then(serde_json::Value::as_str)
            {
                return Some(serde_json::json!({"type": "input_file", "file_id": file_id}));
            }
            let file_data = item.get("base64").and_then(serde_json::Value::as_str)?;
            Some(serde_json::json!({"type": "input_file", "file_data": file_data}))
        }
        _ => Some(item.clone()),
    }
}

fn arguments_to_string(arguments: serde_json::Value) -> String {
    match arguments {
        serde_json::Value::String(text) => text,
        value => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    }
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat};

    #[test]
    fn responses_body_maps_structured_output_and_controls() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.4".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.previous_response_id = Some("resp_prev".to_string());
        opts.store = Some(false);
        opts.background = Some(true);
        opts.truncation = Some("auto".to_string());
        opts.include = Some(vec!["file_search_call.results".to_string()]);
        opts.max_tool_calls = Some(3);
        opts.output_format = OutputFormat::JsonSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {"ok": {"type": "boolean"}},
                "required": ["ok"],
            }),
            strict: true,
        };
        opts.provider_tools = vec![serde_json::json!({
            "type": "web_search_preview"
        })];
        opts.native_tools = Some(vec![serde_json::json!({
            "type": "function",
            "defer_loading": true,
            "function": {
                "name": "lookup",
                "description": "Lookup a record",
                "parameters": {"type": "object"},
            }
        })]);
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_request_body(&payload);

        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert_eq!(body["store"], false);
        assert_eq!(body["background"], true);
        assert_eq!(body["truncation"], "auto");
        assert_eq!(body["include"][0], "file_search_call.results");
        assert_eq!(body["max_tool_calls"], 3);
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(
            body["text"]["format"]["schema"]["properties"]["ok"]["type"],
            "boolean"
        );
        assert_eq!(body["tools"][0]["type"], "web_search_preview");
        assert_eq!(body["tools"][1]["type"], "function");
        assert_eq!(body["tools"][1]["name"], "lookup");
        assert_eq!(body["tools"][1]["defer_loading"], true);
    }

    #[test]
    fn compact_body_keeps_only_compaction_controls() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.4".to_string();
        opts.api_mode = LlmApiMode::Responses;
        opts.system = Some("Preserve actionable state.".to_string());
        opts.previous_response_id = Some("resp_prev".to_string());
        opts.output_format = OutputFormat::JsonObject;
        opts.provider_tools = vec![serde_json::json!({"type": "web_search"})];
        opts.truncation = Some("auto".to_string());
        opts.max_tool_calls = Some(3);
        let payload = LlmRequestPayload::from(&opts);

        let body = OpenAiResponsesProvider::build_compact_request_body(&payload);

        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["instructions"], "Preserve actionable state.");
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert!(body.get("text").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("max_tool_calls").is_none());
    }
}
