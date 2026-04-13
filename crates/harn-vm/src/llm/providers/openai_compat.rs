//! OpenAI-compatible provider — covers OpenAI, OpenRouter, Together, Groq,
//! DeepSeek, Fireworks, HuggingFace, local vLLM/SGLang, and any server that
//! speaks the `/v1/chat/completions` protocol.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

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
        // OpenAI-compatible thinking — set explicitly in both directions
        // so the request is unambiguous about what we want. Qwen-family
        // chat templates default to enabled when this is absent, which
        // means a stage that wants fast tool-call responses gets a
        // thinking-mode pass that swallows the budget. Providers that
        // don't support the field (OpenRouter, Anthropic-routed) strip it
        // via `transform_request()`.
        body["chat_template_kwargs"] = serde_json::json!({
            "enable_thinking": opts.thinking.is_some(),
        });
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
