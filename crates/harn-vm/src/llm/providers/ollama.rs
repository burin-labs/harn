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
        // Start from OpenAI-compatible body (with force_string_content=true for Ollama)
        let mut body =
            crate::llm::providers::OpenAiCompatibleProvider::build_request_body(opts, true);

        // Ollama-specific tuning
        if body["options"].get("num_ctx").is_none() {
            if let Some(num_ctx) = crate::llm::api::ollama_num_ctx_override() {
                body["options"]["num_ctx"] = serde_json::json!(num_ctx);
            }
        }
        if let Some(keep_alive) = crate::llm::api::ollama_keep_alive_override() {
            body["keep_alive"] = keep_alive;
        }
        // Coding agent tuning defaults
        if body["options"].get("min_p").is_none() {
            body["options"]["min_p"] = serde_json::json!(0.05);
        }
        if body["options"].get("repeat_penalty").is_none() {
            body["options"]["repeat_penalty"] = serde_json::json!(1.05);
        }
        if body["options"].get("num_predict").is_none() && opts.max_tokens > 0 {
            body["options"]["num_predict"] = serde_json::json!(opts.max_tokens);
        }
        // Thinking control: Ollama's chat templates (e.g. qwen3:30b-a3b)
        // gate `<think>` emission on the Go-template booleans `$.IsThinkSet`
        // and `.Thinking`, which are populated from the top-level `think`
        // field — NOT from `chat_template_kwargs.enable_thinking`. Even
        // when we drive Ollama through `/v1/chat/completions`, the OpenAI-
        // compat shim passes the `think` extension through to the same
        // template context. Default to `false` so an agent loop turn is
        // fast and tool-call-shaped; callers that want extended reasoning
        // set `thinking` explicitly. (`chat_template_kwargs` is still set
        // by the OpenAI-compat builder for vLLM/SGLang hosts that key off
        // it instead.)
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
