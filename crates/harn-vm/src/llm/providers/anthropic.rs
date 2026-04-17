//! Anthropic Messages API provider (Claude models).

use std::cell::RefCell;
use std::collections::HashSet;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

thread_local! {
    static ANTHROPIC_PREFILL_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static ANTHROPIC_SAMPLING_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    static ANTHROPIC_ADAPTIVE_WARN_ONCE: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
}

/// Parse the (major, minor) generation out of a Claude model ID. Handles
/// both dash-separated names like `claude-opus-4-7` / `claude-sonnet-4-6`
/// and dotted variants like `claude-opus-4.7` (OpenRouter, some proxies),
/// plus dated IDs like `claude-haiku-4-5-20251001`.
///
/// Returns `None` if the ID isn't a known Claude shape (e.g. `gpt-4o`).
fn claude_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    if !lower.starts_with("claude-") && !lower.contains("/claude-") {
        return None;
    }
    // Try dotted form first: "…claude-opus-4.7…", "…claude-opus-4.6-fast…".
    for family in ["opus", "sonnet", "haiku"] {
        let needle = format!("{family}-");
        if let Some(idx) = lower.find(&needle) {
            let tail = &lower[idx + needle.len()..];
            // Dotted: "4.7" / "4.6"
            if let Some((major, rest)) = tail.split_once('.') {
                if let Ok(major) = major.parse::<u32>() {
                    let minor_str: String =
                        rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(minor) = minor_str.parse::<u32>() {
                        return Some((major, minor));
                    }
                }
            }
            // Dashed: "4-7", "4-6", "4-20250514" (minor=0 when the second
            // chunk is clearly a date, not a small version number).
            let mut parts = tail.split('-');
            if let Some(major_str) = parts.next() {
                if let Ok(major) = major_str.parse::<u32>() {
                    if let Some(minor_str) = parts.next() {
                        if let Ok(minor) = minor_str.parse::<u32>() {
                            // Dates ≥ 1000 aren't minor versions.
                            let minor = if minor >= 1000 { 0 } else { minor };
                            return Some((major, minor));
                        }
                    }
                    return Some((major, 0));
                }
            }
        }
    }
    None
}

/// True for Claude 4.6 and later — the generation where Anthropic
/// deprecated the assistant-prefill feature. Opus 4.7, Sonnet 4.6/4.7,
/// any future -4.8+ model all return 400 when the last message has
/// role=assistant.
fn is_claude_4_6_or_later(model: &str) -> bool {
    matches!(claude_generation(model), Some((major, minor)) if (major, minor) >= (4, 6))
}

/// True for Opus 4.7+ — the generation where Anthropic made non-default
/// `temperature`, `top_p`, and `top_k` return HTTP 400. Sonnet/Haiku 4.7
/// will inherit this restriction if they ship with the same API surface.
fn model_rejects_sampling_params(model: &str) -> bool {
    let lower = model.to_lowercase();
    // Apply to every 4.7+ Claude. The migration guide scopes this to Opus
    // 4.7 today, but the family-wide pattern has been consistent and we'd
    // rather drop a non-default sampling param than hit a 400 in prod.
    matches!(claude_generation(&lower), Some((major, minor)) if (major, minor) >= (4, 7))
}

/// True for Opus 4.7+ — the generation where extended thinking was
/// replaced by adaptive thinking. Passing `thinking.type = "enabled"` to
/// one of these models is a 400. We transparently rewrite the payload to
/// `{type: "adaptive"}` and emit a one-time warning.
fn model_requires_adaptive_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    matches!(claude_generation(&lower), Some((major, minor)) if (major, minor) >= (4, 7))
}

fn model_supports_anthropic_prefill(model: &str) -> bool {
    !is_claude_4_6_or_later(model)
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

fn warn_sampling_stripped(model: &str) {
    ANTHROPIC_SAMPLING_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.sampling",
                &format!(
                    "temperature/top_p/top_k supplied for {model}, but Anthropic \
                     Opus 4.7+ rejects non-default sampling params with HTTP 400; \
                     stripping them from the request",
                ),
            );
        }
    });
}

fn warn_adaptive_thinking_rewrite(model: &str) {
    ANTHROPIC_ADAPTIVE_WARN_ONCE.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.insert(model.to_string()) {
            crate::events::log_warn(
                "llm.thinking",
                &format!(
                    "extended-thinking payload supplied for {model}, but Anthropic \
                     Opus 4.7+ removed that surface; rewriting to \
                     `thinking: {{type: adaptive}}` (budget_tokens ignored)",
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
        // Claude Opus 4.7+ rejects non-default sampling parameters with
        // HTTP 400. We strip them transparently and warn once per model
        // so pipeline authors don't have to special-case each release.
        let strip_sampling = model_rejects_sampling_params(&opts.model);
        let any_sampling_supplied =
            opts.temperature.is_some() || opts.top_p.is_some() || opts.top_k.is_some();
        if strip_sampling && any_sampling_supplied {
            warn_sampling_stripped(&opts.model);
        }
        if !strip_sampling {
            if let Some(temp) = opts.temperature {
                body["temperature"] = serde_json::json!(temp);
            }
            if let Some(top_p) = opts.top_p {
                body["top_p"] = serde_json::json!(top_p);
            }
            if let Some(top_k) = opts.top_k {
                body["top_k"] = serde_json::json!(top_k);
            }
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
            // Claude Opus 4.7+ replaced extended thinking with adaptive
            // thinking; `type: enabled` returns HTTP 400. Rewrite the
            // payload transparently rather than fighting the deprecation.
            if model_requires_adaptive_thinking(&opts.model) {
                warn_adaptive_thinking_rewrite(&opts.model);
                body["thinking"] = serde_json::json!({ "type": "adaptive" });
            } else {
                let budget = match thinking {
                    ThinkingConfig::Enabled => 10000,
                    ThinkingConfig::WithBudget(b) => *b,
                };
                body["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
            }
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
