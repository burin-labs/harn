//! Anthropic Messages API provider (Claude models).

use std::cell::RefCell;
use std::collections::HashSet;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

thread_local! {
    static ANTHROPIC_PREFILL_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
}

/// Anthropic deprecated the assistant-prefill feature starting with
/// Claude 4.6. Any model name containing `claude-opus-4-6`,
/// `claude-sonnet-4-6`, or any future `claude-*-4-6[+]` variant drops
/// the prefill to avoid HTTP 400s.
fn model_supports_anthropic_prefill(model: &str) -> bool {
    let lower = model.to_lowercase();
    !(lower.contains("claude-opus-4-6")
        || lower.contains("claude-sonnet-4-6")
        || lower.contains("claude-haiku-4-6")
        || lower.contains("-4.6"))
}

fn warn_anthropic_prefill_skipped(model: &str) {
    ANTHROPIC_PREFILL_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.prefill",
                &format!(
                    "assistant prefill requested for {model}, but Anthropic 4.6+ \
                     deprecated prefill; sending without it",
                ),
            );
        }
    });
}

/// Zero-cost unit struct for the Anthropic provider.
pub(crate) struct AnthropicProvider;

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn is_anthropic_style(&self) -> bool {
        true
    }

    fn supports_cache(&self) -> bool {
        true
    }

    fn supports_thinking(&self) -> bool {
        true
    }
}

impl LlmProviderChat for AnthropicProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl AnthropicProvider {
    /// Build the Anthropic-style request body.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let anthropic_max = if opts.max_tokens > 0 {
            opts.max_tokens
        } else {
            8192
        };
        let mut messages = opts.messages.clone();
        if let Some(ref prefill) = opts.prefill {
            // Claude 4.6+ deprecated the assistant-prefill feature and
            // returns HTTP 400 when the final message is role=assistant.
            // Skip the prefill for those models with a one-time warning
            // rather than fighting the deprecation.
            if model_supports_anthropic_prefill(&opts.model) {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": prefill,
                }));
            } else {
                warn_anthropic_prefill_skipped(&opts.model);
            }
        }
        let mut body = serde_json::json!({
            "model": opts.model,
            "messages": messages,
            "max_tokens": anthropic_max,
        });
        if let Some(ref sys) = opts.system {
            if opts.cache {
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": sys,
                    "cache_control": {"type": "ephemeral"},
                }]);
            } else {
                body["system"] = serde_json::json!(sys);
            }
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = opts.top_k {
            body["top_k"] = serde_json::json!(top_k);
        }
        if let Some(ref stop) = opts.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        if let Some(ref tc) = opts.tool_choice {
            body["tool_choice"] = tc.clone();
        }
        // Anthropic structured output uses a tool-use constraint.
        if opts.response_format.as_deref() == Some("json") {
            if let Some(ref schema) = opts.json_schema {
                body["tools"] = {
                    let mut tools = body["tools"].as_array().cloned().unwrap_or_default();
                    tools.push(serde_json::json!({
                        "name": "json_response",
                        "description": "Return a structured JSON response matching the schema.",
                        "input_schema": schema,
                    }));
                    serde_json::json!(tools)
                };
                body["tool_choice"] = serde_json::json!({"type": "tool", "name": "json_response"});
            }
        }
        if let Some(ref thinking) = opts.thinking {
            let budget = match thinking {
                ThinkingConfig::Enabled => 10000,
                ThinkingConfig::WithBudget(b) => *b,
            };
            body["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget,
            });
        }
        body
    }

    /// The actual chat implementation. Delegates to the shared transport in
    /// `api.rs` after building the provider-specific request body.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        crate::llm::api::vm_call_llm_api_with_body(
            request,
            delta_tx,
            Self::build_request_body(request),
            true,  // is_anthropic_style
            false, // is_ollama
        )
        .await
    }
}
