//! Google Gemini provider.

use std::rc::Rc;

use crate::llm::api::{
    DeltaSender, LlmRequestPayload, LlmResult, ProviderTelemetry, ReasoningEffort, ThinkingConfig,
};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::{VmError, VmValue};

pub(crate) struct GeminiProvider;

impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
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
        for message in &opts.messages {
            let role = match message.get("role").and_then(|value| value.as_str()) {
                Some("assistant" | "model") => "model",
                _ => "user",
            };
            let content = message.get("content").unwrap_or(&serde_json::Value::Null);
            let parts = crate::llm::content::gemini_parts(content);
            if !parts.is_empty() {
                contents.push(serde_json::json!({
                    "role": role,
                    "parts": parts,
                }));
            }
        }

        let mut body = serde_json::json!({ "contents": contents });
        if let Some(system) = opts.system.as_deref().filter(|value| !value.is_empty()) {
            body["system_instruction"] = serde_json::json!({
                "parts": [{"text": system}],
            });
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
        if !generation_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(generation_config);
        }
        if let Some(overrides) = opts
            .provider_overrides
            .as_ref()
            .and_then(|value| value.as_object())
        {
            for (key, value) in overrides {
                body[key] = value.clone();
            }
        }
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
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "gemini API error: {error}"
            ))))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                crate::llm::api::classify_provider_http_error("gemini", status, None, &body)
                    .message,
            ))));
        }
        let json: serde_json::Value = response.json().await.map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "gemini response parse error: {error}"
            ))))
        })?;
        let result = parse_response(&json, request)?;
        if let Some(tx) = delta_tx {
            if !result.text.is_empty() {
                let _ = tx.send(result.text.clone());
            }
        }
        Ok(result)
    }
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
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "gemini API error: {message}"
        )))));
    }
    let mut text = String::new();
    let mut blocks = Vec::new();
    if let Some(parts) = json["candidates"][0]["content"]["parts"].as_array() {
        for part in parts {
            if let Some(fragment) = part.get("text").and_then(|value| value.as_str()) {
                text.push_str(fragment);
                blocks.push(serde_json::json!({
                    "type": "output_text",
                    "text": fragment,
                    "visibility": "public",
                }));
            }
        }
    }
    let input_tokens = json["usageMetadata"]["promptTokenCount"]
        .as_i64()
        .unwrap_or(0);
    let output_tokens = json["usageMetadata"]["candidatesTokenCount"]
        .as_i64()
        .unwrap_or(0);
    let stop_reason = json["candidates"][0]["finishReason"]
        .as_str()
        .map(str::to_string);
    // Gemini's REST response carries only token counts (no server-side
    // timings) so we synthesize an OpenAI-shaped usage block and route it
    // through the same normalizer the rest of the providers use.
    let usage = serde_json::json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
    });
    let request_id = json["responseId"]
        .as_str()
        .filter(|value| !value.is_empty());
    let telemetry = ProviderTelemetry::from_openai_usage(&usage, request_id);
    Ok(LlmResult {
        text,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: request.model.clone(),
        provider: request.provider.clone(),
        thinking: None,
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
    use crate::llm::api::ThinkingConfig;

    fn text_payload(model: &str, thinking: ThinkingConfig) -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "gemini".to_string(),
            model: model.to_string(),
            api_key: String::new(),
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
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            thinking,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            prefill: None,
            session_id: None,
        }
    }

    #[test]
    fn gemini_image_content_maps_to_inline_data() {
        let payload = LlmRequestPayload {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            api_key: String::new(),
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
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            prefill: None,
            session_id: None,
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
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            prefill: None,
            session_id: None,
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
        assert_eq!(body["system_instruction"]["parts"][0]["text"], "system");
    }

    #[test]
    fn gemini_pdf_and_audio_content_maps_to_parts() {
        let payload = LlmRequestPayload {
            provider: "gemini".to_string(),
            model: "gemini-2.5-flash".to_string(),
            api_key: String::new(),
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
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: true,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            prefill: None,
            session_id: None,
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
}
