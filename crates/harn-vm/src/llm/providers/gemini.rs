//! Google Gemini provider.
//!
//! Google serves the Gemini models over two incompatible synchronous wire
//! shapes. This module owns `:generateContent` — the legacy path, the only
//! shape Gemini Batch accepts, and the body/parse pair the Vertex provider
//! delegates to. [`interactions`] owns `/v1beta/interactions`. Which one a
//! route uses is the typed `live_endpoint_family` capability, resolved once in
//! the capability matrix and switched on in exactly one place
//! ([`GeminiProvider::chat_impl`]).

mod generate_content_stream;
pub(crate) mod interactions;
mod interactions_stream;
#[cfg(test)]
mod interactions_tests;
#[cfg(test)]
mod test_support;

use crate::llm::api::{
    DeltaSender, LlmRequestPayload, LlmRequestPayloadExt, LlmResult, OutputFormat,
    ProviderTelemetry, RawProviderToolCall, ReasoningEffort, ThinkingConfig,
};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, google_function_declaration_tools, maybe_emit_delta,
    output_text_block, reasoning_block, tool_call_block,
};
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::{VmError, VmValue};
use generate_content_stream::{generate_content_url, read_generate_content_stream};

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
    ) -> crate::llm::provider::ProviderChatFuture<'a> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

// Per-model Gemini thinking quirks are read from the capability matrix
// (capabilities.toml `[[provider.gemini]]` rows — or `[[provider.vertex]]`
// rows when the Vertex provider delegates its body shaping here) instead of
// hard-coded `model.contains(...)` branches:
//   * thinking-config support  -> the row declares effort in `thinking_modes`
//     (only the gemini-2.5* rows do; gemma / older gemini do not).
//   * can disable thinking     -> `reasoning_disable_supported` (Flash true,
//     Pro false).
//   * high/xhigh budget ceiling -> `max_thinking_budget` (Flash 24576, Pro
//     32768).
fn gemini_supports_thinking_config(caps: &crate::llm::capabilities::Capabilities) -> bool {
    caps.thinking_modes.iter().any(|mode| mode == "effort")
}

fn gemini_max_thinking_budget(caps: &crate::llm::capabilities::Capabilities) -> i64 {
    caps.max_thinking_budget.unwrap_or(32_768)
}

fn gemini_can_disable_thinking(caps: &crate::llm::capabilities::Capabilities) -> bool {
    // Production omits a disable the catalog says the route will 400.
    // The probe sends it anyway so that prediction can be falsified.
    caps.reasoning_disable_supported || !crate::llm::catalog_may_shape_requested_reasoning()
}

fn gemini_thinking_budget(
    caps: &crate::llm::capabilities::Capabilities,
    thinking: &ThinkingConfig,
) -> Option<i64> {
    if !gemini_supports_thinking_config(caps) {
        return None;
    }
    match thinking {
        ThinkingConfig::Disabled => gemini_can_disable_thinking(caps).then_some(0),
        ThinkingConfig::Enabled {
            budget_tokens: Some(tokens),
        } => Some((*tokens).into()),
        ThinkingConfig::Enabled {
            budget_tokens: None,
        }
        | ThinkingConfig::Adaptive => Some(-1),
        ThinkingConfig::Effort { level } => Some(match level {
            ReasoningEffort::None => {
                return gemini_can_disable_thinking(caps).then_some(0);
            }
            ReasoningEffort::Minimal => 1_024,
            ReasoningEffort::Low => 1_024,
            ReasoningEffort::Medium => 8_192,
            ReasoningEffort::High => gemini_max_thinking_budget(caps),
            ReasoningEffort::XHigh => gemini_max_thinking_budget(caps),
            ReasoningEffort::Max => gemini_max_thinking_budget(caps),
        }),
    }
}

impl GeminiProvider {
    pub(crate) fn parse_response(
        json: &serde_json::Value,
        request: &LlmRequestPayload,
    ) -> Result<LlmResult, VmError> {
        parse_response(json, request)
    }

    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let history = harn_llm::wire::gemini::generate_content_history(opts);
        let mut body = serde_json::json!({ "contents": history.contents });
        if !history.system_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({ "parts": history.system_parts });
        }
        let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
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
        if let Some(seed) = opts.seed {
            generation_config.insert("seed".to_string(), serde_json::json!(seed));
        }
        if let Some(fp) = opts.frequency_penalty {
            generation_config.insert("frequencyPenalty".to_string(), serde_json::json!(fp));
        }
        if let Some(pp) = opts.presence_penalty {
            generation_config.insert("presencePenalty".to_string(), serde_json::json!(pp));
        }
        // No capability flag exists for logprobs (mirroring openai_compat,
        // which gates on the payload field alone); Gemini generationConfig
        // spells the pair responseLogprobs (enable) + logprobs (top-K count).
        if let Some(logprobs) = opts.logprobs {
            generation_config.insert("responseLogprobs".to_string(), serde_json::json!(true));
            if let Some(top_logprobs) = logprobs.top {
                generation_config.insert("logprobs".to_string(), serde_json::json!(top_logprobs));
            }
        }
        if let Some(budget) = gemini_thinking_budget(&caps, &opts.thinking) {
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
                generation_config.insert(
                    "responseJsonSchema".to_string(),
                    sanitize_schema_for_provider(
                        &opts.provider,
                        &opts.model,
                        SchemaCompatProfile::Google,
                        SchemaSurface::StructuredOutput,
                        schema,
                    ),
                );
            }
        }
        if !generation_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(generation_config);
        }
        if let Some(tools) = google_function_declaration_tools(
            &opts.provider,
            &opts.model,
            opts.native_tools.as_deref(),
        ) {
            body["tools"] = tools;
        }
        if let Some(tool_config) =
            harn_llm::wire::gemini::generate_content_tool_config(opts.tool_choice.as_ref())
        {
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
        let dialect = crate::llm::api::DialectContract::for_request(request);
        // The one place the two Gemini live endpoint families diverge. Vertex
        // never reaches here — it has its own `chat_impl` and delegates only to
        // `build_request_body` — so its URL/auth envelope stays untouched.
        if dialect.stream_protocol() == crate::llm::api::StreamProtocol::GeminiInteractionsSse {
            return interactions::GeminiInteractions::chat(request, delta_tx).await;
        }
        let pdef = crate::llm_config::provider_config(&request.provider);
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        let wire_model = crate::llm_config::wire_model_id(&request.model);
        let model = wire_model.strip_prefix("models/").unwrap_or(&wire_model);
        let url = generate_content_url(&base_url, model, request.stream);
        let boundary =
            crate::llm::api::inference_boundary::effective_result(request.inference_boundary)
                .map_err(VmError::Runtime)?;
        let approved = crate::llm::api::inference_boundary::ApprovedWireRoute::capture(
            boundary,
            &request.provider,
            &url,
            model,
        );
        let body = dialect.build_request_body(request);
        if let Some(approved) = &approved {
            approved.verify_url_model(&request.provider, &url, model, &body)?;
        }
        let client = if request.stream {
            crate::llm::streaming_client_for_base_url(&base_url)
        } else {
            crate::llm::blocking_client_for_base_url(&base_url)
        };
        let req = client
            .post(url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = crate::llm::api::apply_auth_headers(req, &request.api_key, pdef.as_ref());
        // Same hook the shared OpenAI-compat transport funnel uses
        // (`crate::llm::agent_observe::persist_raw_provider_*`), called
        // directly here because the two live Gemini endpoint families run
        // their own hand-rolled transport rather than that funnel; see
        // `StreamProtocol::GeminiJson | GeminiInteractionsSse =>
        // unreachable!()` in `api/transport.rs`.
        let raw_capture_context = crate::llm::agent_observe::current_raw_provider_capture_context();
        let attempt = request.stream.then_some(0);
        crate::llm::agent_observe::persist_raw_provider_request(
            raw_capture_context.as_ref(),
            "gemini",
            &request.model,
            dialect.wire().as_str(),
            attempt,
            &body,
        );
        let response = req
            .send()
            .await
            .map_err(|error| crate::llm::api::reqwest_send_error("gemini", "API", error))?;
        if !response.status().is_success() {
            let status = response.status();
            let headers = response.headers().clone();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body_text = response.text().await.unwrap_or_default();
            crate::llm::agent_observe::persist_raw_provider_response(
                raw_capture_context.as_ref(),
                "gemini",
                &request.model,
                if request.stream {
                    "stream-error"
                } else {
                    "json"
                },
                attempt,
                status.as_u16(),
                content_type.as_deref(),
                &body_text,
            );
            return Err(crate::llm::api::provider_http_error(
                Some(dialect),
                "gemini",
                status,
                &headers,
                &body_text,
            ));
        }
        if request.stream {
            let envelope = read_generate_content_stream(response, delta_tx, dialect).await?;
            // The wire text itself is not retained past per-line SSE parsing
            // in this reader, so this captures the assembled response
            // envelope Harn actually parsed rather than raw per-chunk bytes.
            crate::llm::agent_observe::persist_raw_provider_response(
                raw_capture_context.as_ref(),
                "gemini",
                &request.model,
                "stream-assembled",
                attempt,
                200,
                Some("application/json"),
                &envelope.to_string(),
            );
            return dialect.parse_response(&envelope, request, false);
        }
        let body_text = response.text().await.map_err(|error| {
            VmError::Thrown(VmValue::string(format!(
                "gemini response read error: {error}"
            )))
        })?;
        crate::llm::agent_observe::persist_raw_provider_response(
            raw_capture_context.as_ref(),
            "gemini",
            &request.model,
            "json",
            attempt,
            200,
            Some("application/json"),
            &body_text,
        );
        let json: serde_json::Value = serde_json::from_str(&body_text).map_err(|error| {
            VmError::Thrown(VmValue::string(format!(
                "gemini response parse error: {error}"
            )))
        })?;
        let result = dialect.parse_response(&json, request, false)?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

pub(crate) use harn_llm::reasoning_history::gemini_tool_call_thought_signature;

/// Parse a Gemini `generateContent` response envelope into an `LlmResult`.
///
/// The Vertex provider delegates here too: Vertex serves the same Gemini
/// models over the same `candidates[].content.parts` / `usageMetadata` shape,
/// so response parsing is single-sourced exactly as request building is
/// (`VertexProvider::build_request_body` delegates to
/// `GeminiProvider::build_request_body`). Keeping one parser is what prevents
/// the two paths from drifting — e.g. losing `thoughtSignature` capture or
/// emitting empty-name tool calls on one path but not the other. The error
/// label and result `provider`/`model` come from `request`, so a Vertex call
/// still surfaces as a Vertex error.
pub(crate) fn parse_response(
    json: &serde_json::Value,
    request: &LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    if let Some(message) = json
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|value| value.as_str())
    {
        return Err(VmError::Thrown(VmValue::string(format!(
            "{} API error: {message}",
            request.provider
        ))));
    }
    let mut text = String::new();
    let mut thinking = String::new();
    let mut blocks = Vec::new();
    let mut tool_calls = Vec::new();
    let mut raw_tool_calls = Vec::new();
    if let Some(parts) = json["candidates"][0]["content"]["parts"].as_array() {
        for (idx, part) in parts.iter().enumerate() {
            let thought_signature = gemini_tool_call_thought_signature(part);
            if let Some(fragment) = part.get("text").and_then(|value| value.as_str()) {
                if part.get("thought").and_then(|value| value.as_bool()) == Some(true) {
                    thinking.push_str(fragment);
                    blocks.push(reasoning_block(fragment));
                } else {
                    text.push_str(fragment);
                    let mut block = output_text_block(fragment);
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
                raw_tool_calls.push(RawProviderToolCall::new(part.clone()).map_err(|message| {
                    VmError::Thrown(VmValue::string(format!(
                        "gemini raw tool call parse error: {message}"
                    )))
                })?);
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
                let mut block = tool_call_block(tool_call["id"].clone(), name, args);
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
    let cache = crate::llm::usage::extract_cache_counts(&json["usageMetadata"])?;
    let stop_reason = json["candidates"][0]["finishReason"]
        .as_str()
        .map(str::to_string);
    let request_id = json["responseId"]
        .as_str()
        .filter(|value| !value.is_empty());
    let mut telemetry = ProviderTelemetry::from_gemini_usage(&json["usageMetadata"], request_id);
    telemetry.billing = crate::llm::usage::BillingUsage::from_gemini(json);
    Ok(LlmResult {
        text,
        raw_tool_calls,
        tool_calls,
        input_tokens,
        output_tokens,
        cache_read_tokens: cache.read_tokens,
        cache_write_tokens: cache.write_tokens,
        cache_counts_reported: cache.reported,
        model: request.model.clone(),
        provider: request.provider.clone(),
        thinking: if thinking.is_empty() {
            None
        } else {
            Some(thinking)
        },
        stop_reason,
        blocks,
        telemetry: Box::new(telemetry),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{OutputFormat, ThinkingConfig};
    use serde_json::json;

    use super::test_support::gemini_payload as text_payload;

    #[test]
    fn parse_response_preserves_raw_function_call_part() {
        let payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "thoughtSignature": "sig-123",
                        "functionCall": {
                            "name": "search",
                            "args": {"query": "rust"}
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5
            }
        });

        let result = parse_response(&response, &payload).expect("gemini response parses");

        assert_eq!(result.tool_calls[0]["name"], "search");
        assert_eq!(result.tool_calls[0]["arguments"]["query"], "rust");
        assert_eq!(result.tool_calls[0]["thought_signature"], "sig-123");
        assert_eq!(result.raw_tool_calls[0]["functionCall"]["name"], "search");
        assert_eq!(
            result.raw_tool_calls[0]["functionCall"]["args"]["query"],
            "rust"
        );
        assert_eq!(result.raw_tool_calls[0]["thoughtSignature"], "sig-123");
        assert!(
            result.raw_tool_calls[0].get("arguments").is_none(),
            "raw Gemini receipt must keep the provider envelope, not Harn's normalized shape"
        );
    }

    #[test]
    fn gemini_image_content_maps_to_inline_data() {
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.vision = true;
        payload.messages = vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "caption"},
                    {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png"}
                ],
        })];
        let body = GeminiProvider::build_request_body(&payload);
        assert_eq!(body["contents"][0]["parts"][0]["text"], "caption");
        assert_eq!(
            body["contents"][0]["parts"][1]["inline_data"],
            serde_json::json!({"mime_type": "image/png", "data": "iVBORw0KGgo="})
        );
    }

    #[test]
    fn gemini_image_url_content_maps_to_file_data() {
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.vision = true;
        payload.messages = vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png"}
                ],
        })];
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
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.vision = true;
        payload.messages = vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "pdf", "base64": "JVBERi0xLjQK"},
                    {"type": "audio", "file_id": "https://generativelanguage.googleapis.com/v1beta/files/abc", "media_type": "audio/mpeg"}
                ],
        })];

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
    fn gemini_sampling_params_map_to_generation_config() {
        // Regression: seed / frequency_penalty / presence_penalty / logprobs
        // were silently dropped by the Gemini request lowering even though the
        // payload carries them and generationConfig supports them.
        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.seed = Some(42);
        payload.frequency_penalty = Some(0.5);
        payload.presence_penalty = Some(-0.25);
        payload.logprobs = Some(crate::llm::api::LogprobsConfig { top: Some(3) });

        let body = GeminiProvider::build_request_body(&payload);
        let config = &body["generationConfig"];
        assert_eq!(config["seed"], 42);
        assert_eq!(config["frequencyPenalty"], 0.5);
        assert_eq!(config["presencePenalty"], -0.25);
        assert_eq!(config["responseLogprobs"], true);
        assert_eq!(config["logprobs"], 3);

        // Unset fields stay omitted (no nulls on the wire).
        let bare = GeminiProvider::build_request_body(&text_payload(
            "gemini-2.5-flash",
            ThinkingConfig::Disabled,
        ));
        let bare_config = bare["generationConfig"].as_object().expect("config");
        for key in [
            "seed",
            "frequencyPenalty",
            "presencePenalty",
            "responseLogprobs",
            "logprobs",
        ] {
            assert!(!bare_config.contains_key(key), "{key} must stay omitted");
        }
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
                        "additionalProperties": true,
                        "properties": {
                            "pattern": {"type": "string"},
                            "query": {
                                "type": "string",
                                "pattern": "^harn",
                                "default": "harn"
                            }
                        },
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
                "additionalProperties": true,
                "properties": {"answer": {"type": "string", "default": "ok"}},
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
        assert!(declarations[0]["parameters"]
            .get("additionalProperties")
            .is_none());
        assert_eq!(
            declarations[0]["parameters"]["properties"]["pattern"]["type"],
            "string"
        );
        assert_eq!(
            declarations[0]["parameters"]["properties"]["query"]["type"],
            "string"
        );
        assert!(declarations[0]["parameters"]["properties"]["query"]
            .get("pattern")
            .is_none());
        assert!(declarations[0]["parameters"]["properties"]["query"]
            .get("default")
            .is_none());
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
        assert!(body["generationConfig"]["responseJsonSchema"]
            .get("additionalProperties")
            .is_none());
        assert!(
            body["generationConfig"]["responseJsonSchema"]["properties"]["answer"]
                .get("default")
                .is_none()
        );
    }

    /// Exercise Native history production through lower encoding, including
    /// assistant text before the signed tool call.
    #[test]
    fn gemini_lowers_producer_built_history_with_text_and_signature() {
        let calls = [json!({
            "id": "call_1",
            "name": "lookup",
            "arguments": {"query": "harn"},
            "thought_signature": "opaque-gemini-signature",
        })];
        let message = crate::llm::tools::build_assistant_tool_message("checking", &calls);

        let mut payload = text_payload("gemini-2.5-flash", ThinkingConfig::Disabled);
        payload.messages = vec![message];

        let body = GeminiProvider::build_request_body(&payload);
        let parts = body["contents"][0]["parts"]
            .as_array()
            .expect("assistant content lowers to a parts array");

        assert_eq!(
            parts.len(),
            2,
            "assistant text and the native call both lower: {parts:?}"
        );
        assert_eq!(parts[0]["text"], "checking");
        assert!(
            parts[0].get("functionCall").is_none(),
            "assistant text keeps its own part: {parts:?}"
        );
        assert_eq!(
            parts[1]["functionCall"],
            json!({"id": "call_1", "name": "lookup", "args": {"query": "harn"}})
        );
        assert_eq!(parts[1]["thoughtSignature"], "opaque-gemini-signature");

        let wire = body.to_string();
        assert!(
            !wire.contains("provider_metadata"),
            "neutral continuation envelope leaked to the wire: {wire}"
        );
        assert!(
            !wire.contains("thought_signature"),
            "neutral signature spelling leaked to the wire: {wire}"
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
        assert!(result.cache_counts_reported);
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
