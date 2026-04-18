//! LLM provider trait and registry.
//!
//! Defines the `LlmProvider` trait that all LLM backends implement, plus a
//! thread-local registry that tracks which providers are available. Provider
//! dispatch happens in `api.rs` via `dispatch_to_registered_provider()`.

use std::cell::RefCell;
use std::collections::HashSet;

use super::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::value::VmError;

/// Trait that all LLM providers implement.
///
/// Dispatch currently goes through concrete types in
/// `api::dispatch_to_registered_provider`. The trait exists so that
/// custom/external providers can be registered at runtime once the
/// `provider_register()` Harn builtin is exposed.
#[allow(dead_code)]
pub(crate) trait LlmProvider {
    /// Provider name (e.g. "anthropic", "openai", "ollama", "mock").
    fn name(&self) -> &str;

    /// Whether this provider uses Anthropic-style messages API (vs OpenAI-style).
    fn is_anthropic_style(&self) -> bool {
        false
    }

    /// Whether this provider supports prompt caching.
    fn supports_cache(&self) -> bool {
        false
    }

    /// Whether this provider supports extended thinking.
    fn supports_thinking(&self) -> bool {
        false
    }

    /// Whether this is the mock provider (deterministic test responses, no API).
    fn is_mock(&self) -> bool {
        false
    }

    /// Whether this is a local provider (e.g. Ollama) that uses NDJSON streaming.
    fn is_local(&self) -> bool {
        false
    }

    /// Whether the provider requires a model to be specified.
    fn requires_model(&self) -> bool {
        true
    }

    /// Apply provider-specific transformations to the request body after it has
    /// been built by `build_request_body()`. Default is a no-op.
    fn transform_request(&self, _body: &mut serde_json::Value) {}

    /// Whether this provider's native API accepts a `defer_loading: true`
    /// flag on tool definitions — keeping their schema out of the model's
    /// context until a tool-search call surfaces them. See Anthropic's tool
    /// search docs and OpenAI's Responses API `tool_search` guide.
    ///
    /// Keyed on the specific model because the capability is model-generation
    /// dependent (e.g. Anthropic: Claude 4.0+ Opus/Sonnet, Haiku 4.5+).
    fn supports_defer_loading(&self, _model: &str) -> bool {
        false
    }

    /// Native tool-search variants this provider supports at the given model.
    /// Return one of:
    ///   - `[]` — no native support; callers must fall back (tracked in #70).
    ///   - `["regex", "bm25"]` — Anthropic's two `tool_search_tool_*_20251119` types.
    ///   - `["hosted", "client"]` — OpenAI Responses API `tool_search` modes.
    ///
    /// Ordering is the provider's recommended default first. Callers that
    /// don't care which variant they get pick element 0.
    fn native_tool_search_variants(&self, _model: &str) -> &'static [&'static str] {
        &[]
    }
}

/// Async chat operation. Uses explicit lifetime parameters because providers
/// are constructed on-the-fly, to avoid RefCell-across-await issues.
#[allow(dead_code)]
pub(crate) trait LlmProviderChat: LlmProvider {
    /// Execute an LLM chat call, optionally streaming text deltas.
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>>;
}

thread_local! {
    /// Thread-local for !Send VM compatibility. Provider objects are
    /// constructed on-the-fly to avoid RefCell-across-await issues.
    static PROVIDER_NAMES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Register all built-in providers. Called once per thread at VM startup.
pub(crate) fn register_default_providers() {
    PROVIDER_NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if !names.is_empty() {
            return;
        }
        names.insert("mock".to_string());
        names.insert("anthropic".to_string());
        names.insert("ollama".to_string());
        for name in [
            "openai",
            "openrouter",
            "together",
            "groq",
            "deepseek",
            "fireworks",
            "huggingface",
            "local",
        ] {
            names.insert(name.to_string());
        }
    });
}

/// Register a custom provider name at runtime.
#[allow(dead_code)]
pub(crate) fn register_provider_name(name: &str) {
    PROVIDER_NAMES.with(|names| {
        names.borrow_mut().insert(name.to_string());
    });
}

/// Check whether a named provider is registered.
pub(crate) fn is_provider_registered(name: &str) -> bool {
    PROVIDER_NAMES.with(|names| names.borrow().contains(name))
}

/// Return all registered provider names (used by diagnostics and tests).
#[allow(dead_code)]
pub(crate) fn registered_provider_names() -> Vec<String> {
    PROVIDER_NAMES.with(|names| names.borrow().iter().cloned().collect())
}

/// Module-level dispatch for `LlmProvider::supports_defer_loading`.
///
/// The VM doesn't carry a trait-object handle for the active provider yet
/// (dispatch is still by string); until it does, this helper keeps the
/// capability logic in one place so callers don't reach into provider
/// structs directly.
pub(crate) fn provider_supports_defer_loading(provider: &str, model: &str) -> bool {
    // `provider_overrides.force_native_tool_search = true` is the escape
    // hatch for users pointed at a proxied OpenAI-compat endpoint (a
    // self-hosted router, an enterprise gateway) whose model ID we
    // cannot parse. The caller consults the override in `options.rs`
    // before falling through here, so this function is the model-
    // detection path only.
    match provider {
        "anthropic" => super::providers::AnthropicProvider.supports_defer_loading(model),
        // OpenAI's native `tool_search` + `defer_loading` land on the
        // Responses API at GPT 5.4+. Every OpenAI-shape provider
        // (OpenAI, OpenRouter, Together, Groq, DeepSeek, Fireworks,
        // HuggingFace, local vLLM) delegates to the same capability
        // check; whether the underlying backend actually forwards the
        // payload is up to that backend. OpenRouter in particular
        // forwards `tool_search` unchanged for upstream OpenAI routes.
        "openai" | "openrouter" | "together" | "groq" | "deepseek" | "fireworks"
        | "huggingface" | "local" => {
            super::providers::OpenAiCompatibleProvider::new(provider.to_string())
                .supports_defer_loading(model)
        }
        // Mock: spoof the real provider whose shape the model ID
        // suggests. This lets conformance tests exercise the native
        // Anthropic/OpenAI paths without making HTTP calls. The mock's
        // tool-capture surface (llm_mock_calls) records the native
        // payload so tests can assert on it directly.
        "mock" => {
            if super::providers::anthropic::claude_model_supports_tool_search(model) {
                true
            } else {
                super::providers::openai_compat::gpt_model_supports_tool_search(model)
            }
        }
        // Everything else: no native support — use the client-executed
        // fallback tracked in harn#70.
        _ => false,
    }
}

/// Module-level dispatch for `LlmProvider::native_tool_search_variants`.
pub(crate) fn provider_tool_search_variants(
    provider: &str,
    model: &str,
) -> &'static [&'static str] {
    match provider {
        "anthropic" => super::providers::AnthropicProvider.native_tool_search_variants(model),
        "openai" | "openrouter" | "together" | "groq" | "deepseek" | "fireworks"
        | "huggingface" | "local" => {
            super::providers::OpenAiCompatibleProvider::new(provider.to_string())
                .native_tool_search_variants(model)
        }
        "mock" => {
            if super::providers::anthropic::claude_model_supports_tool_search(model) {
                super::providers::AnthropicProvider.native_tool_search_variants(model)
            } else {
                super::providers::OpenAiCompatibleProvider::new("openai".to_string())
                    .native_tool_search_variants(model)
            }
        }
        _ => &[],
    }
}

/// Which wire shape to emit for the native tool-search meta-tool. Kept
/// in one place so the options layer, the tools builder, and the
/// response parser all agree on who emits what. Anthropic emits
/// `tool_search_tool_*_20251119` meta-tools; OpenAI-shape providers
/// emit `{"type": "tool_search"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeToolSearchShape {
    /// Anthropic's `{"type": "tool_search_tool_{bm25,regex}_20251119"}`.
    Anthropic,
    /// OpenAI Responses API's `{"type": "tool_search"}`.
    OpenAi,
}
