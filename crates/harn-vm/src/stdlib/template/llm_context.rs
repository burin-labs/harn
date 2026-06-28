//! Ambient render-time LLM context exposed to `.harn.prompt` templates.
//!
//! When `render()` / `render_prompt()` / `render_string()` is invoked from
//! within an LLM-aware frame (`llm_call`, the default handler stack, or
//! `agent_loop`), the active provider/model/family/capabilities are
//! published as the reserved `llm` scope key so authors can write
//! capability-aware partials without manual plumbing:
//!
//! ```text
//! {{ if llm.capabilities.native_tools }}
//!   Call `finish_task` when done.
//! {{ else }}
//!   When done, output: `<<DONE>>`
//! {{ end }}
//! ```
//!
//! Bare `render()` calls outside any LLM frame leave `llm = nil`, so
//! templates branch on `{{ if llm }}` for the doc-gen / CI paths.
//!
//! The context lives in a thread-local stack so concurrent agent_loop
//! iterations on different threads stay isolated; nested
//! push/pop pairs (e.g. an inner `llm_call` from a middleware handler)
//! shadow the outer frame for the duration of the inner render.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::value::VmValue;

/// Resolved provider/model identity plus the corresponding capability
/// snapshot, materialized at LLM-frame entry and injected as the `llm`
/// binding during any `render()` call inside that frame.
#[derive(Debug, Clone)]
pub struct LlmRenderContext {
    pub provider: String,
    pub model: String,
    pub family: String,
    /// Snapshot of `provider_capabilities(provider, model)` — a
    /// `VmValue::Dict` shaped exactly like the builtin's return value.
    pub capabilities: VmValue,
}

impl LlmRenderContext {
    /// Build a context from resolved provider/model strings, looking up
    /// the capability snapshot and deriving the canonical model family.
    pub fn resolve(provider: &str, model: &str) -> Self {
        let caps = crate::llm::capabilities::lookup(provider, model);
        let capabilities =
            crate::llm::config_builtins::capabilities_to_vm_value(provider, model, &caps);
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            family: crate::llm_config::model_family(provider, model),
            capabilities,
        }
    }

    /// Materialize the context as the `llm` scope value:
    /// `{provider, model, family, capabilities: <provider_capabilities dict>}`.
    pub fn to_vm_value(&self) -> VmValue {
        let mut dict = BTreeMap::new();
        dict.put_str("provider", self.provider.as_str());
        dict.put_str("model", self.model.as_str());
        dict.put_str("family", self.family.as_str());
        dict.insert("capabilities".to_string(), self.capabilities.clone());
        VmValue::dict(dict)
    }
}

thread_local! {
    static LLM_RENDER_STACK: RefCell<Vec<LlmRenderContext>> = const { RefCell::new(Vec::new()) };
}

/// Push a frame onto the ambient render-context stack. Pair with
/// `pop_llm_render_context` (or use `LlmRenderContextGuard`) so the
/// stack stays balanced even on the unwind path.
pub fn push_llm_render_context(ctx: LlmRenderContext) {
    LLM_RENDER_STACK.with(|stack| stack.borrow_mut().push(ctx));
}

/// Pop the most recently pushed frame. Returns `None` (rather than
/// panicking) if the stack was empty, since the host may legitimately
/// unwind through a balanced push/pop sequence.
pub fn pop_llm_render_context() -> Option<LlmRenderContext> {
    LLM_RENDER_STACK.with(|stack| stack.borrow_mut().pop())
}

/// Return a clone of the active frame, or `None` if no LLM context is
/// in scope. Render entry-points use this to decide whether to inject
/// the `llm` binding.
pub fn current_llm_render_context() -> Option<LlmRenderContext> {
    LLM_RENDER_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Per-task ambient-scope swap of the LLM render-context stack. See
/// `orchestration::ambient_scope`: this frame is pushed for the duration of an
/// `llm_call` (held across the model HTTP `.await`), so concurrent fan-out
/// children each running their own `llm_call` would otherwise render templates
/// with a sibling's provider/model/capabilities.
pub(crate) fn swap_llm_render_stack(next: Vec<LlmRenderContext>) -> Vec<LlmRenderContext> {
    LLM_RENDER_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

/// Reset the stack — wired into `reset_thread_local_state` so tests
/// and serialized adapter sessions start clean.
pub(crate) fn reset_llm_render_stack() {
    LLM_RENDER_STACK.with(|stack| stack.borrow_mut().clear());
}

/// RAII guard that pushes a context on construction and pops on drop.
/// Use this in Rust hosts (e.g. `llm_call_impl`) so the stack stays
/// balanced across `?`-shortcircuits and panics.
pub struct LlmRenderContextGuard {
    /// Tagged so a misuse (drop-order inversion across nested guards)
    /// surfaces as a `debug_assert` instead of silently popping the
    /// wrong frame. Carries no runtime cost in release builds.
    expected_depth: usize,
}

impl LlmRenderContextGuard {
    pub fn enter(ctx: LlmRenderContext) -> Self {
        push_llm_render_context(ctx);
        let depth = LLM_RENDER_STACK.with(|stack| stack.borrow().len());
        Self {
            expected_depth: depth,
        }
    }
}

impl Drop for LlmRenderContextGuard {
    fn drop(&mut self) {
        let depth = LLM_RENDER_STACK.with(|stack| stack.borrow().len());
        debug_assert_eq!(
            depth, self.expected_depth,
            "LlmRenderContextGuard nested-drop order violated",
        );
        pop_llm_render_context();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive_family(provider: &str, model: &str) -> String {
        crate::llm_config::model_family(provider, model)
    }

    #[test]
    fn family_from_model_id_takes_precedence() {
        assert_eq!(
            derive_family("openrouter", "anthropic/claude-3-5-sonnet"),
            "anthropic-claude"
        );
        assert_eq!(derive_family("openrouter", "openai/gpt-4o"), "openai-gpt");
        assert_eq!(
            derive_family("openrouter", "google/gemini-1.5-pro"),
            "google-gemini"
        );
        assert_eq!(derive_family("llamacpp", "qwen3.6-35b-a3b"), "qwen");
    }

    #[test]
    fn family_falls_back_to_provider_alias() {
        assert_eq!(
            derive_family("anthropic", "unknown-future-model"),
            "anthropic-claude"
        );
        assert_eq!(derive_family("azure", "deployment-xyz"), "openai-gpt");
        assert_eq!(derive_family("vertex", "model-xyz"), "google-gemini");
        assert_eq!(derive_family("local", "anonymous-snapshot"), "local");
        assert_eq!(derive_family("", ""), "unknown");
    }

    #[test]
    fn push_pop_stack_round_trip() {
        reset_llm_render_stack();
        assert!(current_llm_render_context().is_none());
        push_llm_render_context(LlmRenderContext::resolve("anthropic", "claude-3-5-sonnet"));
        assert_eq!(
            current_llm_render_context().map(|c| c.family),
            Some("anthropic-claude".to_string()),
        );
        push_llm_render_context(LlmRenderContext::resolve("openai", "gpt-4o"));
        assert_eq!(
            current_llm_render_context().map(|c| c.family),
            Some("openai-gpt".to_string()),
        );
        pop_llm_render_context();
        assert_eq!(
            current_llm_render_context().map(|c| c.family),
            Some("anthropic-claude".to_string()),
        );
        pop_llm_render_context();
        assert!(current_llm_render_context().is_none());
    }

    #[test]
    fn guard_pops_on_drop() {
        reset_llm_render_stack();
        {
            let _guard = LlmRenderContextGuard::enter(LlmRenderContext::resolve(
                "anthropic",
                "claude-3-5-sonnet",
            ));
            assert!(current_llm_render_context().is_some());
        }
        assert!(current_llm_render_context().is_none());
    }
}
