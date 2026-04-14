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
