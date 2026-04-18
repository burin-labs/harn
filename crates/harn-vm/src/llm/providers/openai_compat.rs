//! OpenAI-compatible provider — covers OpenAI, OpenRouter, Together, Groq,
//! DeepSeek, Fireworks, HuggingFace, local vLLM/SGLang, and any server that
//! speaks the `/v1/chat/completions` protocol.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

/// Parse the (major, minor) version out of a GPT model ID. Handles dotted
/// forms like `gpt-5.4`, `gpt-5.4-preview`, `gpt-5.4-turbo-20260115`, and
/// dashed forms like `gpt-5-4`. Also strips OpenRouter-style prefixes
/// (`openai/gpt-5.4`, `azure/gpt-5.4`) so the same parser can gate
/// capabilities regardless of which OpenAI-compatible provider is routing.
///
/// Returns `None` for non-GPT shapes (`claude-opus-4-7`, `llama-3.1`, …).
pub(crate) fn gpt_generation(model: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    // Strip any `namespace/` prefix (OpenRouter, Azure, vertex).
    let stripped = match lower.rsplit_once('/') {
        Some((_, tail)) => tail,
        None => lower.as_str(),
    };
    let needle = "gpt-";
    let idx = stripped.find(needle)?;
    let tail = &stripped[idx + needle.len()..];
    // Dotted: "5.4" / "5.4-preview" / "5.4-turbo".
    if let Some((major, rest)) = tail.split_once('.') {
        if let Ok(major) = major.parse::<u32>() {
            let minor_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(minor) = minor_str.parse::<u32>() {
                return Some((major, minor));
            }
        }
    }
    // Dashed: "5-4" / "5-4-preview" / "5" (minor=0).
    let mut parts = tail.split('-');
    if let Some(major_str) = parts.next() {
        if let Ok(major) = major_str.parse::<u32>() {
            if let Some(minor_str) = parts.next() {
                if let Ok(minor) = minor_str.parse::<u32>() {
                    // Dates ≥ 1000 are stamps, not minor versions.
                    let minor = if minor >= 1000 { 0 } else { minor };
                    return Some((major, minor));
                }
            }
            return Some((major, 0));
        }
    }
    None
}

/// True for GPT models that expose OpenAI's Responses-API `tool_search` meta-tool
/// and the `defer_loading: true` flag on user tool definitions. Per OpenAI's
/// docs, the feature is gated on GPT 5.4+ (hosted + client-executed modes).
/// We intentionally ignore legacy `gpt-4*`, `gpt-3.5*`, and any non-GPT model;
/// those fall back to the client-executed path from harn#70.
///
/// Retained only as a pure-parse helper for `capabilities::lookup` callers
/// that want to ask the model-ID question without loading the full rule
/// table. The authoritative gate is
/// `capabilities::lookup(provider, model).defer_loading`.
#[allow(dead_code)]
pub(crate) fn gpt_model_supports_tool_search(model: &str) -> bool {
    match gpt_generation(model) {
        Some((major, minor)) => (major, minor) >= (5, 4),
        None => false,
    }
}

/// OpenAI-compatible provider parameterized by name. A single struct handles
/// all OpenAI-style backends — the provider name is used to resolve config
/// (base URL, auth, etc.) from `llm_config`.
pub(crate) struct OpenAiCompatibleProvider {
    provider_name: String,
}

impl OpenAiCompatibleProvider {
    pub(crate) fn new(name: String) -> Self {
        Self {
            provider_name: name,
        }
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    /// Apply provider-specific request body transformations. For OpenRouter,
    /// strip the `chat_template_kwargs` thinking field since OpenRouter does
    /// not reliably support it.
    fn transform_request(&self, body: &mut serde_json::Value) {
        if self.provider_name.to_lowercase().contains("openrouter") {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("chat_template_kwargs");
            }
        }
    }

    // `supports_defer_loading` and `native_tool_search_variants` are
    // served by the default trait impl, which reads `capabilities.toml`.
    // The `gpt_model_supports_tool_search` helper below is retained for
    // shape detection in `helpers/options.rs::classify_native_shape`
    // (deciding Anthropic- vs OpenAI-wire shape for the mock provider).
}

impl LlmProviderChat for OpenAiCompatibleProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl OpenAiCompatibleProvider {
    /// Build the OpenAI-compatible request body.
    pub(crate) fn build_request_body(
        opts: &LlmRequestPayload,
        force_string_content: bool,
    ) -> serde_json::Value {
        let mut msgs = Vec::new();
        if let Some(ref sys) = opts.system {
            msgs.push(serde_json::json!({"role": "system", "content": sys}));
        }
        msgs.extend(opts.messages.iter().cloned());
        if let Some(ref prefill) = opts.prefill {
            msgs.push(serde_json::json!({
                "role": "assistant",
                "content": prefill,
            }));
        }
        msgs = crate::llm::api::normalize_openai_style_messages(msgs, force_string_content);

        let mut body = serde_json::json!({
            "model": opts.model,
            "messages": msgs,
        });
        if opts.max_tokens > 0 {
            body["max_tokens"] = serde_json::json!(opts.max_tokens);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = opts.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(ref stop) = opts.stop {
            body["stop"] = serde_json::json!(stop);
        }
        if let Some(seed) = opts.seed {
            body["seed"] = serde_json::json!(seed);
        }
        if let Some(fp) = opts.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(fp);
        }
        if let Some(pp) = opts.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pp);
        }
        if opts.response_format.as_deref() == Some("json") {
            if let Some(ref schema) = opts.json_schema {
                body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": schema,
                        "strict": true,
                    }
                });
            } else {
                body["response_format"] = serde_json::json!({"type": "json_object"});
            }
        }
        if let Some(ref tools) = opts.native_tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools);
            }
        }
        if let Some(ref tc) = opts.tool_choice {
            body["tool_choice"] = tc.clone();
        }
        // Always set explicitly: Qwen templates default enabled when
        // absent, making fast tool-call turns waste budget on thinking.
        // OpenRouter / Anthropic-routed strip it via transform_request().
        // When prefill is present we also set `add_generation_prompt:
        // false` so vLLM continues the final assistant message instead
        // of starting a fresh assistant turn after it.
        let mut chat_template_kwargs = serde_json::json!({
            "enable_thinking": opts.thinking.is_some(),
        });
        if opts.prefill.is_some() {
            chat_template_kwargs["add_generation_prompt"] = serde_json::json!(false);
            chat_template_kwargs["continue_final_message"] = serde_json::json!(true);
        }
        body["chat_template_kwargs"] = chat_template_kwargs;
        body
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let mut body = Self::build_request_body(request, false);
        self.transform_request(&mut body);
        crate::llm::api::vm_call_llm_api_with_body(
            request, delta_tx, body, false, // is_anthropic_style
            false, // is_ollama
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_search_supported_for_gpt_5_4_and_up() {
        assert!(gpt_model_supports_tool_search("gpt-5.4"));
        assert!(gpt_model_supports_tool_search("gpt-5.4-preview"));
        assert!(gpt_model_supports_tool_search("gpt-5.4-turbo"));
        assert!(gpt_model_supports_tool_search("gpt-5-4"));
        assert!(gpt_model_supports_tool_search("gpt-5.5"));
        assert!(gpt_model_supports_tool_search("gpt-6.0"));
    }

    #[test]
    fn tool_search_unsupported_for_pre_5_4() {
        assert!(!gpt_model_supports_tool_search("gpt-4o"));
        assert!(!gpt_model_supports_tool_search("gpt-4.1"));
        assert!(!gpt_model_supports_tool_search("gpt-4-turbo"));
        assert!(!gpt_model_supports_tool_search("gpt-3.5-turbo"));
        assert!(!gpt_model_supports_tool_search("gpt-5.0"));
        assert!(!gpt_model_supports_tool_search("gpt-5.3-preview"));
        assert!(!gpt_model_supports_tool_search("gpt-5"));
    }

    #[test]
    fn tool_search_unsupported_for_non_gpt() {
        assert!(!gpt_model_supports_tool_search("claude-opus-4-7"));
        assert!(!gpt_model_supports_tool_search("llama-3.1-70b"));
        assert!(!gpt_model_supports_tool_search(""));
    }

    #[test]
    fn gpt_generation_handles_openrouter_prefix() {
        // OpenRouter model IDs carry an `openai/` prefix. Same capability
        // check must produce the same answer.
        assert_eq!(gpt_generation("openai/gpt-5.4-preview"), Some((5, 4)));
        assert_eq!(gpt_generation("azure/gpt-5.5-turbo"), Some((5, 5)));
        assert!(gpt_model_supports_tool_search("openai/gpt-5.4"));
        assert!(!gpt_model_supports_tool_search("openai/gpt-4o"));
    }

    #[test]
    fn gpt_generation_ignores_date_suffix_as_minor() {
        // `gpt-5-20260115` should parse as generation (5, 0), not (5, 20260115).
        assert_eq!(gpt_generation("gpt-5-20260115"), Some((5, 0)));
        assert!(!gpt_model_supports_tool_search("gpt-5-20260115"));
    }

    #[test]
    fn native_tool_search_variants_lists_hosted_first() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        let variants = provider.native_tool_search_variants("gpt-5.4-preview");
        assert_eq!(variants, vec!["hosted".to_string(), "client".to_string()]);
    }

    #[test]
    fn native_tool_search_variants_empty_for_old_model() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        assert!(provider.native_tool_search_variants("gpt-4o").is_empty());
    }

    #[test]
    fn supports_defer_loading_matches_tool_search_gate() {
        let provider = OpenAiCompatibleProvider::new("openai".to_string());
        assert!(provider.supports_defer_loading("gpt-5.4"));
        assert!(!provider.supports_defer_loading("gpt-4o"));
    }
}
