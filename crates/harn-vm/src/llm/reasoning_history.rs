//! Canonical capture and provider-wire lowering for reasoning history.
//!
//! The durable transcript keeps readable reasoning separately. This module
//! owns the narrower provider-visible continuation data: signed Anthropic
//! blocks and typed same-field OpenAI-compatible replay.

use crate::llm::capabilities::{ReasoningHistoryWireField, ReasoningRoundTripPolicy};

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

pub(crate) fn capture_anthropic_block(block: &serde_json::Value) -> Option<serde_json::Value> {
    is_signed_anthropic_block(block).then(|| block.clone())
}

pub(crate) fn capture_anthropic_blocks(
    blocks: &[serde_json::Value],
    enabled: bool,
) -> Vec<serde_json::Value> {
    if !enabled {
        return Vec::new();
    }
    blocks.iter().filter_map(capture_anthropic_block).collect()
}

pub(crate) fn is_signed_anthropic_block(block: &serde_json::Value) -> bool {
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

pub(crate) fn should_replay_anthropic_block(
    block: &serde_json::Value,
    policy: ReasoningRoundTripPolicy,
) -> bool {
    policy == ReasoningRoundTripPolicy::EchoSigned && is_signed_anthropic_block(block)
}

/// Preserve exact signed blocks ahead of the visible/tool content in an
/// Anthropic assistant history message. The provider requires the original
/// order and bytes when the next turn continues that reasoning context.
pub(crate) fn prepend_signed_anthropic_blocks(
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
pub(crate) fn attach_anthropic_continuation(
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
pub(crate) fn restore_anthropic_continuation(
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

pub(crate) fn openai_same_key_reasoning(
    message: &serde_json::Value,
    policy: ReasoningRoundTripPolicy,
    field: Option<ReasoningHistoryWireField>,
) -> Option<(&'static str, serde_json::Value)> {
    if policy != ReasoningRoundTripPolicy::EchoSameKey
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

#[derive(Debug, Default)]
pub(crate) struct AnthropicThinkingBlock {
    thinking: String,
    signature: String,
}

impl AnthropicThinkingBlock {
    pub(crate) fn from_start(block: &serde_json::Value) -> Self {
        Self {
            thinking: block
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            signature: block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    pub(crate) fn push_thinking(&mut self, value: &str) {
        self.thinking.push_str(value);
    }

    pub(crate) fn push_signature(&mut self, value: &str) {
        self.signature.push_str(value);
    }

    pub(crate) fn finish(self) -> serde_json::Value {
        if self.signature.is_empty() {
            serde_json::json!({
                "type": "reasoning",
                "text": self.thinking,
                "visibility": "private",
            })
        } else {
            serde_json::json!({
                "type": "thinking",
                "thinking": self.thinking,
                "signature": self.signature,
            })
        }
    }
}
