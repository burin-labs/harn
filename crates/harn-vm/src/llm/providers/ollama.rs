//! Ollama provider — local Ollama server with NDJSON streaming.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ThinkingConfig};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

/// Zero-cost unit struct for the Ollama provider.
pub(crate) struct OllamaProvider;

impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn is_local(&self) -> bool {
        true
    }
}

impl LlmProviderChat for OllamaProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl OllamaProvider {
    /// Build the Ollama-specific request body. Ollama uses OpenAI-style messages
    /// but with additional options and NDJSON streaming.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let mut body =
            crate::llm::providers::OpenAiCompatibleProvider::build_request_body(opts, true);

        if body["options"].get("num_ctx").is_none() {
            if let Some(num_ctx) = crate::llm::api::ollama_num_ctx_override() {
                body["options"]["num_ctx"] = serde_json::json!(num_ctx);
            }
        }
        if let Some(keep_alive) = crate::llm::api::ollama_keep_alive_override() {
            body["keep_alive"] = keep_alive;
        }
        if body["options"].get("min_p").is_none() {
            body["options"]["min_p"] = serde_json::json!(0.05);
        }
        if body["options"].get("repeat_penalty").is_none() {
            body["options"]["repeat_penalty"] = serde_json::json!(1.05);
        }
        if body["options"].get("num_predict").is_none() && opts.max_tokens > 0 {
            body["options"]["num_predict"] = serde_json::json!(opts.max_tokens);
        }
        // Ollama templates (qwen3:30b-a3b etc.) gate `<think>` emission
        // on the top-level `think` field, NOT
        // `chat_template_kwargs.enable_thinking`. The OpenAI-compat shim
        // passes `think` through to the same template context. Default
        // false for fast tool-call-shaped turns; callers who want
        // reasoning set `thinking` explicitly.
        body["think"] = match opts.thinking {
            Some(ThinkingConfig::Enabled) | Some(ThinkingConfig::WithBudget(_)) => {
                serde_json::json!(true)
            }
            None => serde_json::json!(false),
        };
        body
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let body = Self::build_request_body(request);
        crate::llm::api::vm_call_llm_api_with_body(
            request, delta_tx, body, false, // is_anthropic_style
            true,  // is_ollama
        )
        .await
    }
}
