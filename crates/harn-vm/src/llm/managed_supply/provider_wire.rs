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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedStreamOptions {
    pub include_usage: bool,
}

/// Exact third-party request body. There is intentionally no metadata field:
/// request identity, routing, and accounting belong to the hosted envelope and
/// authoritative receipt, not to a provider-specific extension surface.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HostedOpenAiRequest {
    pub model: String,
    pub messages: Vec<HostedChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<HostedFunctionTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<HostedToolChoice>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<HostedStreamOptions>,
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
        if matches!(&message.content, HostedContent::Text(text) if text.trim().is_empty()) {
            return Err(ManagedSupplyContractError::new(
                "hosted chat text content must not be empty",
            ));
        }
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
    let provider_definition = crate::llm_config::provider_config(provider).ok_or_else(|| {
        ManagedSupplyContractError::new("hosted chat provider is not in Harn's registry")
    })?;
    let catalog_model = crate::llm_config::model_catalog_entry(model)
        .filter(|entry| entry.provider == provider)
        .ok_or_else(|| {
            ManagedSupplyContractError::new(
                "hosted chat model is not a canonical route for the selected provider",
            )
        })?;
    let wire_model = catalog_model
        .wire_model
        .unwrap_or_else(|| model.to_string());
    let stream_options = (request.stream
        && provider_definition.stream_usage_accounting == Some(true))
    .then_some(HostedStreamOptions {
        include_usage: true,
    });
    Ok(HostedOpenAiRequest {
        model: wire_model,
        messages: request.messages,
        tools: request.tools,
        tool_choice: request.tool_choice,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        stream: request.stream,
        stream_options,
    })
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
            hosted_openai_request("groq", "qwen/qwen3.6-27b", request(true)).expect("Groq request"),
        )
        .expect("JSON");
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert!(value.get("metadata").is_none());
        assert!(value.get("harn_managed_supply").is_none());
    }

    #[test]
    fn together_projection_omits_undocumented_stream_usage_extension() {
        let value = serde_json::to_value(
            hosted_openai_request(
                "together",
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                request(true),
            )
            .expect("Together request"),
        )
        .expect("JSON");
        assert!(value.get("stream_options").is_none());
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
            hosted_openai_request("groq", "qwen/qwen3.6-27b", request(false))
                .expect("Groq request"),
        )
        .expect("JSON");
        assert_eq!(value["model"], "qwen/qwen3.6-27b");
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
        assert!(hosted_openai_request("groq", "model", request).is_err());
    }
}
