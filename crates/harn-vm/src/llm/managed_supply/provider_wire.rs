//! Closed OpenAI-compatible chat contract for managed provider supply.
//!
//! Harn owns the provider protocol. Hosted envelopes may add tenancy, budget,
//! and audit fields around this contract, but those fields never enter the
//! physical provider request produced here.

use serde::{Deserialize, Serialize};

use super::ManagedSupplyContractError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

/// OpenAI-compatible message content is either text, a closed content-part
/// array, or null on an assistant message that contains tool calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostedContent {
    Text(String),
    Parts(Vec<HostedContentPart>),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostedContentPart {
    Text { text: String },
    ImageUrl { image_url: HostedImageUrl },
    InputAudio { input_audio: HostedInputAudio },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<HostedImageDetail>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedImageDetail {
    Auto,
    Low,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedInputAudio {
    pub data: String,
    pub format: HostedAudioFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedAudioFormat {
    Wav,
    Mp3,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: HostedToolKind,
    pub function: HostedToolCallFunction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedToolKind {
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedChatMessage {
    pub role: HostedRole,
    pub content: HostedContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<HostedToolCall>,
    /// Provider reasoning round-trip payload. This is the only known
    /// provider extension Harn deliberately carries through message history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedFunctionDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema is recursively open by definition. Construction validates
    /// that this value is an object before any provider request is emitted.
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedFunctionTool {
    #[serde(rename = "type")]
    pub kind: HostedToolKind,
    pub function: HostedFunctionDefinition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostedToolChoice {
    Mode(HostedToolChoiceMode),
    Function(HostedNamedToolChoice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedToolChoiceMode {
    Auto,
    None,
    Required,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedNamedToolChoice {
    #[serde(rename = "type")]
    pub kind: HostedToolKind,
    pub function: HostedNamedFunction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedNamedFunction {
    pub name: String,
}

/// Provider-neutral chat input transported through a managed gateway.
/// Physical provider identity and hosted accounting remain outside this type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedChatRequest {
    pub messages: Vec<HostedChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<HostedFunctionTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<HostedToolChoice>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub stream: bool,
}

/// Provider body built from closed input by the ordinary transport encoder.
/// Serialize this value directly; its fields cannot be changed after validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HostedOpenAiRequest {
    body: serde_json::Value,
}

fn nonempty(value: &str, field: &str) -> Result<(), ManagedSupplyContractError> {
    if value.trim().is_empty() {
        return Err(ManagedSupplyContractError::new(format!(
            "hosted chat {field} must not be empty"
        )));
    }
    Ok(())
}

impl HostedChatRequest {
    /// Validate invariants that Serde's closed shape cannot express by itself.
    pub fn validate(&self) -> Result<(), ManagedSupplyContractError> {
        validate_request(self)
    }
}

fn validate_request(request: &HostedChatRequest) -> Result<(), ManagedSupplyContractError> {
    if request.messages.is_empty() || request.max_tokens == 0 || !request.temperature.is_finite() {
        return Err(ManagedSupplyContractError::new(
            "hosted chat requires messages, positive max_tokens, and finite temperature",
        ));
    }
    for message in &request.messages {
        // Empty strings carry text-tool history and empty-file tool results.
        // Preserve their data exactly as the ordinary provider transport does.
        if matches!(&message.content, HostedContent::Null) && message.tool_calls.is_empty() {
            return Err(ManagedSupplyContractError::new(
                "hosted chat null content requires assistant tool calls",
            ));
        }
        if let HostedContent::Parts(parts) = &message.content {
            if parts.is_empty() {
                return Err(ManagedSupplyContractError::new(
                    "hosted chat content parts must not be empty",
                ));
            }
            for part in parts {
                match part {
                    HostedContentPart::Text { text } => nonempty(text, "content text")?,
                    HostedContentPart::ImageUrl { image_url } => {
                        nonempty(&image_url.url, "image URL")?;
                    }
                    HostedContentPart::InputAudio { input_audio } => {
                        nonempty(&input_audio.data, "input audio")?;
                    }
                }
            }
        }
        if matches!(message.role, HostedRole::Tool) {
            nonempty(
                message.tool_call_id.as_deref().unwrap_or_default(),
                "tool_call_id",
            )?;
        }
        for call in &message.tool_calls {
            nonempty(&call.id, "tool call id")?;
            nonempty(&call.function.name, "tool call function name")?;
        }
    }
    for tool in &request.tools {
        nonempty(&tool.function.name, "tool name")?;
        if !tool.function.parameters.is_object() {
            return Err(ManagedSupplyContractError::new(
                "hosted chat tool parameters must be a JSON Schema object",
            ));
        }
    }
    Ok(())
}

/// Lower one managed chat request to the exact OpenAI-compatible provider
/// body selected by Harn's provider registry.
pub fn hosted_openai_request(
    provider: &str,
    model: &str,
    request: HostedChatRequest,
) -> Result<HostedOpenAiRequest, ManagedSupplyContractError> {
    nonempty(provider, "provider")?;
    nonempty(model, "model")?;
    validate_request(&request)?;
    crate::llm_config::provider_config(provider).ok_or_else(|| {
        ManagedSupplyContractError::new("hosted chat provider is not in Harn's registry")
    })?;
    crate::llm_config::model_catalog_entry(model)
        .filter(|entry| entry.provider == provider)
        .ok_or_else(|| {
            ManagedSupplyContractError::new(
                "hosted chat model is not a canonical route for the selected provider",
            )
        })?;
    let payload = request.into_payload(provider, model)?;
    let dialect = crate::llm::api::DialectContract::for_request(&payload);
    if dialect.wire() != crate::llm::capabilities::WireDialect::OpenAiCompat {
        return Err(ManagedSupplyContractError::new(
            "hosted chat requires an OpenAI-compatible provider route",
        ));
    }
    let mut body = dialect.encode_request_body(&payload);
    dialect.apply_stream_transport_fields(&mut body, provider, payload.stream);
    Ok(HostedOpenAiRequest { body })
}

impl HostedChatRequest {
    fn into_payload(
        self,
        provider: &str,
        model: &str,
    ) -> Result<crate::llm::api::LlmRequestPayload, ManagedSupplyContractError> {
        let serialization_error = |error: serde_json::Error| {
            ManagedSupplyContractError::new(format!("hosted chat encoding failed: {error}"))
        };
        Ok(crate::llm::api::LlmRequestPayload {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: self
                .messages
                .into_iter()
                .map(|mut message| -> Result<_, serde_json::Error> {
                    let reasoning = message.reasoning_content.take();
                    let mut message = serde_json::to_value(message)?;
                    if let Some(reasoning) = reasoning {
                        message["reasoning"] = serde_json::Value::String(reasoning);
                    }
                    Ok(message)
                })
                .collect::<Result<_, _>>()
                .map_err(serialization_error)?,
            native_tools: if self.tools.is_empty() {
                None
            } else {
                Some(
                    self.tools
                        .into_iter()
                        .map(serde_json::to_value)
                        .collect::<Result<_, _>>()
                        .map_err(serialization_error)?,
                )
            },
            tool_choice: self
                .tool_choice
                .map(serde_json::to_value)
                .transpose()
                .map_err(serialization_error)?,
            max_tokens: i64::from(self.max_tokens),
            temperature: Some(f64::from(self.temperature)),
            stream: self.stream,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(stream: bool) -> HostedChatRequest {
        HostedChatRequest {
            messages: vec![HostedChatMessage {
                role: HostedRole::User,
                content: HostedContent::Text("hello".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            }],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 32,
            temperature: 0.2,
            stream,
        }
    }

    #[test]
    fn groq_projection_is_closed_and_requests_terminal_usage() {
        let value = serde_json::to_value(
            hosted_openai_request("groq", "qwen/qwen3.8-27b", request(true)).expect("Groq request"),
        )
        .expect("JSON");
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert!(value.get("metadata").is_none());
        assert!(value.get("harn_managed_supply").is_none());
    }

    #[test]
    fn together_projection_requests_terminal_usage() {
        let value = serde_json::to_value(
            hosted_openai_request(
                "together",
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                request(true),
            )
            .expect("Together request"),
        )
        .expect("JSON");
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn deepseek_projection_requests_terminal_usage() {
        let value = serde_json::to_value(
            hosted_openai_request("deepseek", "deepseek-v4-flash", request(true))
                .expect("DeepSeek request"),
        )
        .expect("JSON");
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn hosted_projection_resolves_catalog_id_to_provider_wire_model() {
        let value = serde_json::to_value(
            hosted_openai_request("groq", "groq/openai/gpt-oss-120b", request(false))
                .expect("Groq GPT-OSS request"),
        )
        .expect("JSON");
        assert_eq!(value["model"], "openai/gpt-oss-120b");
    }

    #[test]
    fn hosted_projection_preserves_same_id_model() {
        let value = serde_json::to_value(
            hosted_openai_request("groq", "qwen/qwen3.8-27b", request(false))
                .expect("Groq request"),
        )
        .expect("JSON");
        assert_eq!(value["model"], "qwen/qwen3.8-27b");
    }

    #[test]
    fn hosted_projection_uses_the_routes_token_limit_field() {
        let value = serde_json::to_value(
            hosted_openai_request("moonshot", "moonshot/kimi-k3", request(true))
                .expect("Moonshot request"),
        )
        .expect("JSON");
        assert_eq!(value["max_completion_tokens"], 32);
        assert!(value.get("max_tokens").is_none());
    }

    #[test]
    fn hosted_projection_omits_tool_choice_without_tools() {
        let mut input = request(false);
        input.tool_choice = Some(HostedToolChoice::Mode(HostedToolChoiceMode::Auto));
        let value = serde_json::to_value(
            hosted_openai_request("groq", "qwen/qwen3.8-27b", input).expect("Groq request"),
        )
        .expect("JSON");
        assert!(value.get("tool_choice").is_none());
    }

    #[test]
    fn hosted_projection_preserves_provider_call_receipts() {
        crate::llm::reset_reasoning_receipts();
        let payload = request(false)
            .into_payload("groq", "qwen/qwen3.8-27b")
            .expect("native input");
        let dialect = crate::llm::api::DialectContract::for_request(&payload);
        let native = dialect.build_request_body(&payload);
        assert_eq!(native["model"], "qwen/qwen3.8-27b");
        let before = crate::llm::peek_reasoning_receipts();
        assert_eq!(before.len(), 1, "native transport records its encoded call");

        for stream in [false, true] {
            let hosted = hosted_openai_request("groq", "qwen/qwen3.8-27b", request(stream))
                .expect("hosted projection");
            assert_eq!(
                serde_json::to_value(hosted).expect("JSON")["model"],
                "qwen/qwen3.8-27b"
            );
        }
        assert_eq!(crate::llm::peek_reasoning_receipts(), before);
        assert_eq!(crate::llm::dropped_reasoning_receipts(), 0);

        dialect.build_request_body(&payload);
        let after = crate::llm::peek_reasoning_receipts();
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].index, 1);
        crate::llm::reset_reasoning_receipts();
    }

    #[test]
    fn hosted_projection_matches_native_tools_history_and_streaming() {
        for (provider, model) in [
            ("groq", "qwen/qwen3.8-27b"),
            ("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
            ("deepseek", "deepseek-v4-flash"),
            ("moonshot", "moonshot/kimi-k3"),
        ] {
            for stream in [false, true] {
                let mut input: HostedChatRequest = serde_json::from_value(serde_json::json!({
                    "messages": [
                        {"role": "user", "content": "Read the test file"},
                        {"role": "assistant", "content": "", "reasoning_content": "Inspect it",
                            "tool_calls": [{"id": "call-read", "type": "function",
                                "function": {"name": "look", "arguments": "{}"}}]},
                        {"role": "tool", "content": "file contents", "name": "look",
                            "tool_call_id": "call-read"}
                    ],
                    "tools": [{"type": "function", "function": {
                        "name": "look", "description": "Read a file", "strict": true,
                        "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                    }}],
                    "tool_choice": "auto", "max_tokens": 32, "temperature": 0.2, "stream": false
                }))
                .expect("closed input");
                input.stream = stream;
                let mut messages = serde_json::to_value(&input.messages)
                    .expect("native messages")
                    .as_array()
                    .expect("array")
                    .clone();
                messages[1]
                    .as_object_mut()
                    .expect("assistant")
                    .remove("reasoning_content");
                messages[1]["reasoning"] = serde_json::json!("Inspect it");
                let payload = crate::llm::api::LlmRequestPayload {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    messages,
                    native_tools: Some(
                        serde_json::to_value(&input.tools)
                            .expect("native tools")
                            .as_array()
                            .expect("array")
                            .clone(),
                    ),
                    tool_choice: Some(serde_json::json!("auto")),
                    max_tokens: 32,
                    temperature: Some(f64::from(0.2_f32)),
                    stream,
                    ..Default::default()
                };
                let dialect = crate::llm::api::DialectContract::for_request(&payload);
                let mut native = dialect.build_request_body(&payload);
                dialect.apply_stream_transport_fields(&mut native, provider, stream);
                let hosted = serde_json::to_value(
                    hosted_openai_request(provider, model, input).expect("hosted input"),
                )
                .expect("JSON");
                assert_eq!(hosted, native, "{provider}/{model}, stream={stream}");
                assert!(hosted.get("metadata").is_none());
                assert!(hosted.get("harn_managed_supply").is_none());
                assert_eq!(hosted["messages"][2]["tool_call_id"], "call-read");
                assert_eq!(hosted["tools"][0]["function"]["name"], "look");
                if matches!(provider, "deepseek" | "moonshot") {
                    assert_eq!(hosted["messages"][1]["reasoning_content"], "Inspect it");
                } else {
                    assert!(hosted["messages"][1].get("reasoning_content").is_none());
                }
                if stream {
                    assert_eq!(hosted["stream_options"]["include_usage"], true);
                } else {
                    assert!(hosted.get("stream_options").is_none());
                }
            }
        }
    }

    #[test]
    fn hosted_projection_rejects_other_provider_dialects() {
        let error = hosted_openai_request("anthropic", "claude-haiku-4-5-20251001", request(false))
            .expect_err("Anthropic messages cannot be sent as OpenAI chat");
        assert!(error
            .to_string()
            .contains("OpenAI-compatible provider route"));
    }

    #[test]
    fn hosted_projection_rejects_provider_model_mismatch() {
        let error = hosted_openai_request("groq", "deepseek-v4-flash", request(false))
            .expect_err("cross-provider route must fail before egress");
        assert!(error
            .to_string()
            .contains("not a canonical route for the selected provider"));
    }

    #[test]
    fn hosted_projection_accepts_text_tool_history() {
        let history = harn_llm::wire::openai::messages::enforce_single_tool_call_history(
            vec![
                serde_json::json!({"role": "user", "content": "Read the test file"}),
                serde_json::json!({
                    "role": "assistant", "content": "",
                    "tool_calls": [{"id": "call-read", "type": "function",
                        "function": {"name": "look", "arguments": "{}"}}]
                }),
                serde_json::json!({"role": "tool", "content": "file contents",
                    "name": "look", "tool_call_id": "call-read"}),
            ],
            false,
        );
        assert_eq!(
            history[1],
            serde_json::json!({"role": "assistant", "content": ""})
        );
        assert_eq!(
            history[2],
            serde_json::json!({"role": "user", "content": "file contents"})
        );
        let mut input = request(false);
        input.messages = serde_json::from_value(serde_json::Value::Array(history.clone()))
            .expect("ordinary text-tool history is a closed hosted message shape");
        let output =
            hosted_openai_request("fireworks", "accounts/fireworks/models/gpt-oss-120b", input)
                .expect("the hosted projection must accept the ordinary transport's history");
        assert_eq!(
            serde_json::to_value(output).expect("JSON")["messages"],
            serde_json::Value::Array(history)
        );
    }

    #[test]
    fn closed_message_contract_rejects_unknown_fields() {
        let error = serde_json::from_value::<HostedChatMessage>(serde_json::json!({
            "role": "user",
            "content": "hello",
            "metadata": {"leak": true}
        }))
        .expect_err("unknown message field must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn hosted_projection_preserves_empty_tool_result_but_requires_its_call_id() {
        let mut input = request(false);
        input.tools.push(HostedFunctionTool {
            kind: HostedToolKind::Function,
            function: HostedFunctionDefinition {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                strict: None,
            },
        });
        input.messages.push(HostedChatMessage {
            role: HostedRole::Assistant,
            content: HostedContent::Null,
            name: None,
            tool_call_id: None,
            tool_calls: vec![HostedToolCall {
                id: "call-empty-file".to_string(),
                kind: HostedToolKind::Function,
                function: HostedToolCallFunction {
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            reasoning_content: None,
        });
        let tool = HostedChatMessage {
            role: HostedRole::Tool,
            content: HostedContent::Text(String::new()),
            name: Some("read_file".to_string()),
            tool_call_id: Some("call-empty-file".to_string()),
            tool_calls: Vec::new(),
            reasoning_content: None,
        };
        input.messages.push(tool.clone());
        let output = hosted_openai_request(
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            input.clone(),
        )
        .expect("an empty tool result is valid history data");
        assert_eq!(
            serde_json::to_value(output).expect("JSON")["messages"][2],
            serde_json::to_value(&tool).expect("JSON")
        );

        input.messages.last_mut().expect("tool result").tool_call_id = Some(String::new());
        let error =
            hosted_openai_request("fireworks", "accounts/fireworks/models/gpt-oss-120b", input)
                .expect_err("empty text must not bypass the required tool-call identity");
        assert!(error.to_string().contains("tool_call_id"));
    }

    #[test]
    fn tool_schema_must_be_an_object() {
        let mut request = request(false);
        request.tools.push(HostedFunctionTool {
            kind: HostedToolKind::Function,
            function: HostedFunctionDefinition {
                name: "search".to_string(),
                description: None,
                parameters: serde_json::json!([]),
                strict: None,
            },
        });
        let error = hosted_openai_request("groq", "qwen/qwen3.8-27b", request)
            .expect_err("the schema guard must reject malformed parameters on a valid route");
        assert!(error.to_string().contains("JSON Schema object"));
    }
}
