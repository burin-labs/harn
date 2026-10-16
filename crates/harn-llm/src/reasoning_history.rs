//! Canonical capture and provider-wire lowering for reasoning history.
//!
//! The durable transcript keeps readable reasoning separately. This module
//! owns the narrower provider-visible continuation data: signed Anthropic
//! blocks and typed same-field OpenAI-compatible replay.

use harn_provider_catalog::capabilities::{ReasoningHistoryWireField, ReasoningRoundTripPolicy};

/// Gemini's opaque thought signature from a live response or retained metadata.
pub fn gemini_tool_call_thought_signature(value: &serde_json::Value) -> Option<&str> {
    value
        .get("thoughtSignature")
        .or_else(|| value.get("thought_signature"))
        .or_else(|| {
            value
                .get("provider_metadata")
                .and_then(|metadata| metadata.get("gemini"))
                .and_then(|gemini| {
                    gemini
                        .get("thought_signature")
                        .or_else(|| gemini.get("thoughtSignature"))
                })
        })
        .and_then(serde_json::Value::as_str)
        .filter(|signature| !signature.is_empty())
}

const PROVIDER_CONTINUATION_KEY: &str = "provider_continuation";

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ProviderContinuation {
    #[serde(skip_serializing_if = "Option::is_none")]
    anthropic: Option<AnthropicContinuation>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct AnthropicContinuation {
    content_blocks: Vec<serde_json::Value>,
}

pub fn capture_anthropic_block(block: &serde_json::Value) -> Option<serde_json::Value> {
    is_signed_anthropic_block(block).then(|| block.clone())
}

pub fn capture_anthropic_blocks(
    blocks: &[serde_json::Value],
    enabled: bool,
) -> Vec<serde_json::Value> {
    if !enabled {
        return Vec::new();
    }
    blocks.iter().filter_map(capture_anthropic_block).collect()
}

pub fn is_signed_anthropic_block(block: &serde_json::Value) -> bool {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("thinking") => {
            block
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && block
                    .get("signature")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        }
        Some("redacted_thinking") => block
            .get("data")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        _ => false,
    }
}

/// Whether this stored envelope is exactly the Anthropic continuation shape
/// captured by this module. Conservative admission may account for this
/// metadata only when egress can restore known signed thinking blocks without
/// accepting arbitrary provider or multimodal payloads.
pub fn is_supported_anthropic_continuation(value: &serde_json::Value) -> bool {
    let Some(provider) = value.as_object() else {
        return false;
    };
    if provider.len() != 1 || !provider.contains_key("anthropic") {
        return false;
    }
    let Some(anthropic) = provider["anthropic"].as_object() else {
        return false;
    };
    if anthropic.len() != 1 || !anthropic.contains_key("content_blocks") {
        return false;
    }
    let Some(blocks) = anthropic["content_blocks"].as_array() else {
        return false;
    };
    !blocks.is_empty() && blocks.iter().all(supported_signed_anthropic_block)
}

fn supported_signed_anthropic_block(block: &serde_json::Value) -> bool {
    let Some(fields) = block.as_object() else {
        return false;
    };
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("thinking") => {
            fields.len() == 3
                && fields.contains_key("thinking")
                && fields.contains_key("signature")
                && block
                    .get("thinking")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                && block
                    .get("signature")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|signature| !signature.is_empty())
        }
        Some("redacted_thinking") => {
            fields.len() == 2
                && fields.contains_key("data")
                && block
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|data| !data.is_empty())
        }
        _ => false,
    }
}

pub fn should_replay_anthropic_block(
    block: &serde_json::Value,
    policy: ReasoningRoundTripPolicy,
) -> bool {
    policy == ReasoningRoundTripPolicy::EchoSigned && is_signed_anthropic_block(block)
}

/// Preserve exact signed blocks ahead of the visible/tool content in an
/// Anthropic assistant history message. The provider requires the original
/// order and bytes when the next turn continues that reasoning context.
pub fn prepend_signed_anthropic_blocks(
    message: &mut serde_json::Value,
    blocks: &[serde_json::Value],
) {
    let signed = blocks
        .iter()
        .filter_map(capture_anthropic_block)
        .collect::<Vec<_>>();
    if signed.is_empty() {
        return;
    }
    let Some(object) = message.as_object_mut() else {
        return;
    };
    let content = object
        .entry("content".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let prior = std::mem::replace(content, serde_json::Value::Null);
    let mut combined = signed;
    match prior {
        serde_json::Value::Array(mut blocks) => combined.append(&mut blocks),
        serde_json::Value::String(text) if !text.is_empty() => {
            combined.push(serde_json::json!({"type": "text", "text": text}));
        }
        serde_json::Value::Null | serde_json::Value::String(_) => {}
        other => combined.push(other),
    }
    *content = serde_json::Value::Array(combined);
}

/// Attach opaque provider-bound continuation state without mixing it into the
/// provider-neutral conversation content.
pub fn attach_anthropic_continuation(
    message: &mut serde_json::Value,
    blocks: &[serde_json::Value],
) {
    let content_blocks = blocks
        .iter()
        .filter_map(capture_anthropic_block)
        .collect::<Vec<_>>();
    if content_blocks.is_empty() {
        return;
    }
    let continuation = ProviderContinuation {
        anthropic: Some(AnthropicContinuation { content_blocks }),
    };
    if let Ok(value) = serde_json::to_value(continuation) {
        message[PROVIDER_CONTINUATION_KEY] = value;
    }
}

/// Restore exact Anthropic continuation blocks only on Anthropic egress, then
/// remove the storage envelope before the request is serialized.
pub fn restore_anthropic_continuation(
    message: &mut serde_json::Value,
    policy: ReasoningRoundTripPolicy,
) {
    let Some(object) = message.as_object_mut() else {
        return;
    };
    let Some(raw) = object.remove(PROVIDER_CONTINUATION_KEY) else {
        return;
    };
    if policy != ReasoningRoundTripPolicy::EchoSigned {
        return;
    }
    let Ok(continuation) = serde_json::from_value::<ProviderContinuation>(raw) else {
        return;
    };
    if let Some(anthropic) = continuation.anthropic {
        prepend_signed_anthropic_blocks(message, &anthropic.content_blocks);
    }
}

pub fn openai_same_key_reasoning(
    message: &serde_json::Value,
    policy: ReasoningRoundTripPolicy,
    field: Option<ReasoningHistoryWireField>,
    request_has_tools: bool,
) -> Option<(&'static str, serde_json::Value)> {
    let replay_allowed = match policy {
        ReasoningRoundTripPolicy::EchoSameKey => true,
        ReasoningRoundTripPolicy::EchoSameKeyWhenToolsPresent => request_has_tools,
        ReasoningRoundTripPolicy::Strip | ReasoningRoundTripPolicy::EchoSigned => false,
    };
    if !replay_allowed
        || message.get("role").and_then(serde_json::Value::as_str) != Some("assistant")
    {
        return None;
    }
    let field = field?;
    let reasoning = message
        .get("reasoning")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    Some((
        field.as_str(),
        serde_json::Value::String(reasoning.to_owned()),
    ))
}

/// Harn's block for an Anthropic `thinking` content block. A block with a
/// signature keeps it so the block can be replayed; an unsigned one cannot be
/// replayed to Anthropic and becomes private reasoning text.
pub fn anthropic_thinking_block(block: &serde_json::Value) -> serde_json::Value {
    let thinking = block
        .get("thinking")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match block
        .get("signature")
        .and_then(serde_json::Value::as_str)
        .filter(|signature| !signature.is_empty())
    {
        Some(signature) => serde_json::json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": signature,
        }),
        None => serde_json::json!({
            "type": "reasoning",
            "text": thinking,
            "visibility": "private",
        }),
    }
}
