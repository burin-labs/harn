//! Google Gemini provider.

use crate::llm::api::{
    DeltaSender, LlmRequestPayload, LlmResult, OutputFormat, ProviderTelemetry, ReasoningEffort,
    ThinkingConfig,
};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, google_function_declaration_tools, maybe_emit_delta,
};
use crate::value::{VmError, VmValue};

pub(crate) struct GeminiProvider;

impl LlmProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }
}

impl LlmProviderChat for GeminiProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

fn gemini_supports_thinking_config(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("gemini-2.5")
}

fn gemini_max_thinking_budget(model: &str) -> i64 {
    if model.to_ascii_lowercase().contains("flash") {
        24_576
    } else {
        32_768
    }
}

fn gemini_can_disable_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("flash") || model.contains("robotics")
}

fn gemini_thinking_budget(model: &str, thinking: &ThinkingConfig) -> Option<i64> {
    if !gemini_supports_thinking_config(model) {
        return None;
    }
    match thinking {
        ThinkingConfig::Disabled => gemini_can_disable_thinking(model).then_some(0),
        ThinkingConfig::Enabled {
            budget_tokens: Some(tokens),
        } => Some((*tokens).into()),
        ThinkingConfig::Enabled {
            budget_tokens: None,
        }
        | ThinkingConfig::Adaptive => Some(-1),
        ThinkingConfig::Effort { level } => Some(match level {
            ReasoningEffort::None => {
                return gemini_can_disable_thinking(model).then_some(0);
            }
            ReasoningEffort::Minimal => 1_024,
            ReasoningEffort::Low => 1_024,
            ReasoningEffort::Medium => 8_192,
            ReasoningEffort::High => gemini_max_thinking_budget(model),
            ReasoningEffort::XHigh => gemini_max_thinking_budget(model),
        }),
    }
}

impl GeminiProvider {
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let mut contents = Vec::new();
        let mut system_parts = Vec::new();
        for message in &opts.messages {
            let role = message
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("user");
            if role == "system" {
                let parts = gemini_message_parts(message);
                if !parts.is_empty() {
                    system_parts.extend(parts);
                }
                continue;
            }
            let gemini_role = match role {
                "assistant" | "model" => "model",
                _ => "user",
            };
            let parts = gemini_message_parts(message);
            if !parts.is_empty() {
                contents.push(serde_json::json!({
                    "role": gemini_role,
                    "parts": parts,
                }));
            }
        }

        let mut body = serde_json::json!({ "contents": contents });
        if let Some(system) = opts.system.as_deref().filter(|value| !value.is_empty()) {
            system_parts.insert(0, serde_json::json!({"text": system}));
        }
        if !system_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({ "parts": system_parts });
        }
        let mut generation_config = serde_json::Map::new();
        if opts.max_tokens > 0 {
            generation_config.insert(
                "maxOutputTokens".to_string(),
                serde_json::json!(opts.max_tokens),
            );
        }
        if let Some(temp) = opts.temperature {
            generation_config.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(top_p) = opts.top_p {
            generation_config.insert("topP".to_string(), serde_json::json!(top_p));
        }
        if let Some(top_k) = opts.top_k {
            generation_config.insert("topK".to_string(), serde_json::json!(top_k));
        }
        if let Some(stop) = &opts.stop {
            generation_config.insert("stopSequences".to_string(), serde_json::json!(stop));
        }
        if let Some(budget) = gemini_thinking_budget(&opts.model, &opts.thinking) {
            generation_config.insert(
                "thinkingConfig".to_string(),
                serde_json::json!({ "thinkingBudget": budget }),
            );
        }
        match &opts.output_format {
            OutputFormat::Text => {}
            OutputFormat::JsonObject => {
                generation_config.insert(
                    "responseMimeType".to_string(),
                    serde_json::json!("application/json"),
                );
            }
            OutputFormat::JsonSchema { schema, .. } => {
                generation_config.insert(
                    "responseMimeType".to_string(),
                    serde_json::json!("application/json"),
                );
                generation_config.insert("responseJsonSchema".to_string(), schema.clone());
            }
        }
        if !generation_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(generation_config);
        }
        if let Some(tools) = google_function_declaration_tools(opts.native_tools.as_deref()) {
            body["tools"] = tools;
        }
        if let Some(tool_config) = gemini_tool_config(opts.tool_choice.as_ref()) {
            body["toolConfig"] = tool_config;
        }
        apply_provider_overrides(&mut body, opts.provider_overrides.as_ref());
        body
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let body = Self::build_request_body(request);
        let pdef = crate::llm_config::provider_config(&request.provider);
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        let model = request
            .model
            .strip_prefix("models/")
            .unwrap_or(&request.model);
        let url = format!("{base_url}/v1beta/models/{model}:generateContent");
        let client = crate::llm::shared_blocking_client().clone();
        let req = client
            .post(url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = crate::llm::api::apply_auth_headers(req, &request.api_key, pdef.as_ref());
        let response = req.send().await.map_err(|error| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "gemini API error: {error}"
            ))))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                crate::llm::api::classify_provider_http_error("gemini", status, None, &body)
                    .message,
            ))));
        }
        let json: serde_json::Value = response.json().await.map_err(|error| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "gemini response parse error: {error}"
            ))))
        })?;
        let result = parse_response(&json, request)?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

fn gemini_message_parts(message: &serde_json::Value) -> Vec<serde_json::Value> {
    if message
        .get("role")
        .and_then(|value| value.as_str())
        .is_some_and(|role| role == "tool" || role == "tool_result")
    {
        if let Some(part) = gemini_function_response_part(message) {
            return vec![part];
        }
    }

    let mut parts = message
        .get("content")
        .map(crate::llm::content::gemini_parts)
        .unwrap_or_default();
    if let Some(calls) = message.get("tool_calls").and_then(|value| value.as_array()) {
        for call in calls {
            if let Some(part) = gemini_function_call_part(call) {
                parts.push(part);
            }
        }
    }
    parts
}

fn gemini_function_call_part(call: &serde_json::Value) -> Option<serde_json::Value> {
    let function = call.get("function").unwrap_or(call);
    let name = function
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    let args = function
        .get("arguments")
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .or_else(|| (!value.is_string()).then(|| value.clone()))
        })
        .or_else(|| call.get("arguments").cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut function_call = serde_json::json!({
        "name": name,
        "args": args,
    });
    if let Some(id) = call
        .get("id")
        .or_else(|| function.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
    {
        function_call["id"] = serde_json::json!(id);
    }
    let mut part = serde_json::json!({ "functionCall": function_call });
    if let Some(signature) = call
        .get("thought_signature")
        .or_else(|| call.get("thoughtSignature"))
        .and_then(serde_json::Value::as_str)
        .filter(|signature| !signature.is_empty())
    {
        part["thoughtSignature"] = serde_json::json!(signature);
    }
    Some(part)
}

fn gemini_function_response_part(message: &serde_json::Value) -> Option<serde_json::Value> {
    let name = message
        .get("name")
        .or_else(|| message.get("tool_name"))
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    let response = message
        .get("content")
        .map(gemini_function_response_payload)
        .unwrap_or_else(|| serde_json::json!({}));
    let mut function_response = serde_json::json!({
        "name": name,
        "response": response,
    });
    if let Some(id) = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
    {
        function_response["id"] = serde_json::json!(id);
    }
    Some(serde_json::json!({ "functionResponse": function_response }))
}

fn gemini_function_response_payload(content: &serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Object(_) => content.clone(),
        serde_json::Value::String(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(object)) => serde_json::Value::Object(object),
            Ok(value) => serde_json::json!({ "result": value }),
            Err(_) => serde_json::json!({ "result": text }),
        },
        serde_json::Value::Null => serde_json::json!({}),
        other => serde_json::json!({ "result": other }),
    }
}

fn gemini_tool_config(tool_choice: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let choice = tool_choice?;
    let mode = match choice {
        serde_json::Value::String(value) => match value.as_str() {
            "none" => "NONE",
            "required" | "any" => "ANY",
            _ => "AUTO",
        },
        serde_json::Value::Object(object) => {
            match object.get("type").and_then(|value| value.as_str()) {
                Some("none") => "NONE",
                Some("function" | "tool" | "any" | "required") => "ANY",
                _ => "AUTO",
            }
        }
        _ => "AUTO",
    };
    let mut config = serde_json::json!({ "functionCallingConfig": { "mode": mode } });
    if mode == "ANY" {
        if let Some(name) = choice
            .get("function")
            .and_then(|value| value.get("name"))
            .or_else(|| choice.get("name"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
        {
            config["functionCallingConfig"]["allowedFunctionNames"] = serde_json::json!([name]);
        }
    }
    Some(config)
}

fn parse_response(
    json: &serde_json::Value,
    request: &LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    if let Some(message) = json
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|value| value.as_str())
    {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("gemini API error: {message}"),
        ))));
    }
    let mut text = String::new();
    let mut thinking = String::new();
    let mut blocks = Vec::new();
    let mut tool_calls = Vec::new();
    if let Some(parts) = json["candidates"][0]["content"]["parts"].as_array() {
        for (idx, part) in parts.iter().enumerate() {
            let thought_signature = part
                .get("thoughtSignature")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty());
            if let Some(fragment) = part.get("text").and_then(|value| value.as_str()) {
                if part.get("thought").and_then(|value| value.as_bool()) == Some(true) {
                    thinking.push_str(fragment);
                    blocks.push(serde_json::json!({
                        "type": "reasoning",
                        "text": fragment,
                        "visibility": "private",
                    }));
                } else {
                    text.push_str(fragment);
                    let mut block = serde_json::json!({
                        "type": "output_text",
                        "text": fragment,
                        "visibility": "public",
                    });
                    if let Some(signature) = thought_signature {
                        block["provider_metadata"] = serde_json::json!({
                            "gemini": {"thought_signature": signature}
                        });
                    }
                    blocks.push(block);
                }
            }
            if let Some(call) = part.get("functionCall") {
                let Some(name) = call
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let args = call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let id = call
                    .get("id")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("gemini_tool_{}", tool_calls.len()));
                let mut tool_call = serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": args.clone(),
                });
                if let Some(signature) = thought_signature {
                    tool_call["thought_signature"] = serde_json::json!(signature);
                }
                tool_calls.push(tool_call.clone());
                let mut block = serde_json::json!({
                    "type": "tool_call",
                    "id": tool_call["id"].clone(),
                    "name": name,
                    "arguments": args,
                    "visibility": "internal",
                });
                if let Some(signature) = thought_signature {
                    block["thought_signature"] = serde_json::json!(signature);
                }
                block["part_index"] = serde_json::json!(idx);
                blocks.push(block);
            }
        }
    }
    let input_tokens = json["usageMetadata"]["promptTokenCount"]
        .as_i64()
        .unwrap_or(0);
    let output_tokens = json["usageMetadata"]["candidatesTokenCount"]
        .as_i64()
        .unwrap_or(0)
        + json["usageMetadata"]["thoughtsTokenCount"]
            .as_i64()
            .unwrap_or(0);
    let cache_read_tokens = json["usageMetadata"]["cachedContentTokenCount"]
        .as_i64()
        .unwrap_or(0);
    let stop_reason = json["candidates"][0]["finishReason"]
        .as_str()
        .map(str::to_string);
    let request_id = json["responseId"]
        .as_str()
        .filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_gemini_usage(&json["usageMetadata"], request_id);
    Ok(LlmResult {
        served_fast: false,
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
        cache_supported: true,
        model: request.model.clone(),
        provider: request.provider.clone(),
        thinking: if thinking.is_empty() {
            None
        } else {
            Some(thinking)
        },
        thinking_summary: None,
        stop_reason,
        blocks,
        logprobs: Vec::new(),
        telemetry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{OutputFormat, ThinkingConfig};
    use serde_json::json;

    fn text_payload(model: &str, thinking: ThinkingConfig) -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "gemini".to_string(),
            model: model.to_string(),
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": "hello",
            })],
            system: None,
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
        }
    }

    #[test]
    fn gemini_image_content_maps_to_inline_data() {
        let payload = LlmRequestPayload {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "caption"},
                    {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png"}
                ],
            })],
            system: None,
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
        };
        let body = GeminiProvider::build_request_body(&payload);
        assert_eq!(body["contents"][0]["parts"][0]["text"], "caption");
        assert_eq!(
            body["contents"][0]["parts"][1]["inline_data"],
            serde_json::json!({"mime_type": "image/png", "data": "iVBORw0KGgo="})
        );
    }

    #[test]
    fn gemini_image_url_content_maps_to_file_data() {
        let mut payload = LlmRequestPayload {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png"}
                ],
            })],
            system: None,
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
        };
        payload.system = Some("system".to_string());

        let body = GeminiProvider::build_request_body(&payload);
        assert_eq!(
            body["contents"][0]["parts"][0]["file_data"],
            serde_json::json!({
                "mime_type": "image/png",
                "file_uri": "https://example.com/image.png",
            })
        );
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "system");
    }

    #[test]
    fn gemini_pdf_and_audio_content_maps_to_parts() {
        let payload = LlmRequestPayload {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "pdf", "base64": "JVBERi0xLjQK"},
                    {"type": "audio", "file_id": "https://generativelanguage.googleapis.com/v1beta/files/abc", "media_type": "audio/mpeg"}
                ],
            })],
            system: None,
            max_tokens: 64,
            temperature: None,
            top_p: None,
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
        };

        let body = GeminiProvider::build_request_body(&payload);
        assert_eq!(
            body["contents"][0]["parts"][0]["inline_data"],
            serde_json::json!({"mime_type": "application/pdf", "data": "JVBERi0xLjQK"})
        );
        assert_eq!(
            body["contents"][0]["parts"][1]["file_data"],
            serde_json::json!({
                "mime_type": "audio/mpeg",
                "file_uri": "https://generativelanguage.googleapis.com/v1beta/files/abc",
            })
        );
    }

    #[test]
    fn gemini_thinking_config_maps_from_typed_thinking() {
        let dynamic = GeminiProvider::build_request_body(&text_payload(
            "gemini-2.5-flash",
            ThinkingConfig::Adaptive,
        ));
        assert_eq!(
            dynamic["generationConfig"]["thinkingConfig"],
            serde_json::json!({"thinkingBudget": -1})
        );

        let flash_high = GeminiProvider::build_request_body(&text_payload(
            "gemini-2.5-flash",
            ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::High,
            },
        ));
        assert_eq!(
            flash_high["generationConfig"]["thinkingConfig"],
            serde_json::json!({"thinkingBudget": 24576})
        );

        let minimal = GeminiProvider::build_request_body(&text_payload(
            "gemini-2.5-flash",
            ThinkingConfig::Effort {
                level: crate::llm::api::ReasoningEffort::Minimal,
            },
        ));
        assert_eq!(
            minimal["generationConfig"]["thinkingConfig"],
            serde_json::json!({"thinkingBudget": 1024})
        );

        let pro_disabled = GeminiProvider::build_request_body(&text_payload(
            "gemini-2.5-pro",
            ThinkingConfig::Disabled,
        ));
        assert!(pro_disabled["generationConfig"]
            .as_object()
            .is_some_and(|config| !config.contains_key("thinkingConfig")));

        let legacy = GeminiProvider::build_request_body(&text_payload(
            "gemini-1.5-pro",
            ThinkingConfig::Enabled {
                budget_tokens: Some(4096),
            },
        ));
        assert!(legacy["generationConfig"]
            .as_object()
            .is_some_and(|config| !config.contains_key("thinkingConfig")));
    }

    #[test]
    fn gemini_native_tools_and_structured_output_map_to_generate_content() {
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.native_tools = Some(vec![
            json!({
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup records",
                    "parameters": {
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "summarize",
                    "parameters": {"type": "object"}
                }
            }),
        ]);
        payload.tool_choice = Some(json!({
            "type": "function",
            "function": {"name": "lookup"}
        }));
        payload.output_format = OutputFormat::JsonSchema {
            schema: json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}},
                "required": ["answer"]
            }),
            strict: true,
        };

        let body = GeminiProvider::build_request_body(&payload);

        let declarations = body["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("function declarations");
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0]["name"], "lookup");
        assert_eq!(
            declarations[0]["parameters"]["properties"]["query"]["type"],
            "string"
        );
        assert_eq!(
            body["toolConfig"]["functionCallingConfig"],
            json!({"mode": "ANY", "allowedFunctionNames": ["lookup"]})
        );
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            body["generationConfig"]["responseJsonSchema"]["properties"]["answer"]["type"],
            "string"
        );
    }

    #[test]
    fn gemini_history_maps_function_calls_and_tool_results() {
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.messages = vec![
            json!({
                "role": "assistant",
                "content": [
                    {
                        "functionCall": {
                            "id": "call_1",
                            "name": "lookup",
                            "args": {"query": "harn"}
                        },
                        "thoughtSignature": "opaque-signature"
                    }
                ]
            }),
            json!({
                "role": "tool",
                "name": "lookup",
                "tool_call_id": "call_1",
                "content": "{\"result\":\"ok\"}"
            }),
        ];

        let body = GeminiProvider::build_request_body(&payload);

        assert_eq!(body["contents"][0]["role"], "model");
        assert_eq!(
            body["contents"][0]["parts"][0]["functionCall"],
            json!({"id": "call_1", "name": "lookup", "args": {"query": "harn"}})
        );
        assert_eq!(
            body["contents"][0]["parts"][0]["thoughtSignature"],
            "opaque-signature"
        );
        assert_eq!(body["contents"][1]["role"], "user");
        assert_eq!(
            body["contents"][1]["parts"][0]["functionResponse"],
            json!({"id": "call_1", "name": "lookup", "response": {"result": "ok"}})
        );
    }

    #[test]
    fn gemini_response_extracts_text_function_calls_signatures_and_cache_usage() {
        let request = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        let response = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "checking"},
                    {
                        "functionCall": {
                            "id": "call_1",
                            "name": "lookup",
                            "args": {"query": "harn"}
                        },
                        "thoughtSignature": "sig-1"
                    },
                    {
                        "functionCall": {
                            "name": "summarize",
                            "args": {"topic": "tools"}
                        }
                    }
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 3,
                "thoughtsTokenCount": 5,
                "cachedContentTokenCount": 7
            },
            "responseId": "resp-1"
        });

        let result = parse_response(&response, &request).expect("gemini response parses");

        assert_eq!(result.text, "checking");
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0]["id"], "call_1");
        assert_eq!(result.tool_calls[0]["name"], "lookup");
        assert_eq!(result.tool_calls[0]["arguments"]["query"], "harn");
        assert_eq!(result.tool_calls[0]["thought_signature"], "sig-1");
        assert_eq!(result.tool_calls[1]["id"], "gemini_tool_1");
        // output_tokens must fold in thinking tokens: candidates(3) + thoughts(5).
        assert_eq!(result.output_tokens, 8);
        assert_eq!(result.input_tokens, 10);
        assert_eq!(result.cache_read_tokens, 7);
        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::GEMINI_USAGE
        );
        assert_eq!(result.telemetry.request_id.as_deref(), Some("resp-1"));
        assert!(result.blocks.iter().any(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("tool_call")
                && block
                    .get("thought_signature")
                    .and_then(|value| value.as_str())
                    == Some("sig-1")
        }));
    }
}
