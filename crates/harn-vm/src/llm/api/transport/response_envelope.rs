//! Provider response facts accumulated while a streaming body is consumed.
//!
//! The SSE state machine owns framing and content assembly. This module owns
//! the smaller diagnostic contract shared by its provider dialects: body
//! response identity, declared content-block shape, and the typed error built
//! when a completed response contains no usable generation.

use crate::llm::api::response::{
    empty_generation_error, openai_reasoning_field_present, ProviderResponseEnvelope,
};
use crate::llm::api::{DialectContract, StreamProtocol};
use crate::llm::usage::ProviderUsageReceipt;
use crate::value::VmError;

#[derive(Default)]
pub(super) struct StreamingResponseEnvelope {
    response_id: Option<String>,
    content_block_types: Vec<String>,
}

pub(super) struct EmptyGenerationContext<'a> {
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) dialect: DialectContract,
    pub(super) body_response_id_fallback: Option<&'a str>,
    pub(super) stop_reason: Option<&'a str>,
    pub(super) output_tokens: i64,
}

impl StreamingResponseEnvelope {
    /// Capture only response-body identity. HTTP request IDs remain provider
    /// telemetry and must never be projected as a response ID.
    pub(super) fn observe_frame(&mut self, frame: &serde_json::Value) {
        if let Some(id) = frame
            .get("id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                frame
                    .pointer("/message/id")
                    .and_then(serde_json::Value::as_str)
            })
            .filter(|value| !value.is_empty())
        {
            self.response_id = Some(id.to_string());
        }
    }

    /// Anthropic starts each logical content block explicitly, so every start
    /// contributes one block, including repeated types.
    pub(super) fn observe_anthropic_block(&mut self, block: &serde_json::Value) {
        self.content_block_types.push(
            block
                .get("type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("object")
                .to_string(),
        );
    }

    /// OpenAI-compatible chunks are fragments of one logical message. Record
    /// each declared channel once, retaining present-but-empty channels.
    pub(super) fn observe_openai_delta(&mut self, delta: &serde_json::Value) {
        if delta.get("content").is_some() {
            self.push_distinct("text");
        }
        if openai_reasoning_field_present(delta) {
            self.push_distinct("reasoning");
        }
        if delta.get("refusal").is_some_and(|value| !value.is_null()) {
            self.push_distinct("refusal");
        }
    }

    pub(super) fn into_empty_generation_error(
        self,
        context: EmptyGenerationContext<'_>,
        usage: ProviderUsageReceipt,
    ) -> VmError {
        let wire_style = if context.dialect.stream_protocol() == StreamProtocol::AnthropicSse {
            "anthropic-native"
        } else {
            "openai-compatible"
        };
        empty_generation_error(
            context.provider,
            context.model,
            ProviderResponseEnvelope::new(
                self.response_id
                    .as_deref()
                    .or(context.body_response_id_fallback),
                context.stop_reason,
                self.content_block_types,
                usage,
            ),
            format!(
                "{wire_style} model {}:{} reported completion_tokens={} but delivered no content, reasoning, or tool calls",
                context.provider, context.model, context.output_tokens
            ),
        )
    }

    fn push_distinct(&mut self, block_type: &str) {
        if !self
            .content_block_types
            .iter()
            .any(|existing| existing == block_type)
        {
            self.content_block_types.push(block_type.to_string());
        }
    }
}
