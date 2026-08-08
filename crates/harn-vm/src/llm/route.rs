//! Typed route resolution for an LLM dispatch.
//!
//! A `(provider, model)` pair plus the request's thinking configuration
//! resolves to a [`Route`] carrying the wire dialect. [`Route::resolve`] is the
//! one place allowed to reject a structurally-broken pairing *before* any HTTP
//! call is made.
//!
//! The bug class this closes (harn#3956): a thinking-enabled Anthropic-family
//! (Claude) model dispatched over the OpenAI-compatible transport. Anthropic's
//! OpenAI-compatibility surface bills the thinking budget in `completion_tokens`
//! but never streams the extended-thinking deltas
//! (<https://platform.claude.com/docs/en/api/openai-sdk>), so the completion
//! comes back billed-but-empty and the transport throws far downstream with no
//! structured cause. The usual trigger is a dropped or mis-scoped provider on an
//! escalation path: `capabilities::lookup("fireworks", "claude-sonnet-4-6")`
//! finds no Anthropic rule (rules are provider-scoped) and defaults to the
//! OpenAI-compatible wire dialect.
//!
//! Resolving this at dispatch entry, over the *actual* `(provider, model,
//! thinking)` the caller is about to send, turns a silently-served empty
//! completion into a loud, typed error that surfaces the upstream
//! provider-drop instead of masking it with a silent string rewrite.

use crate::llm::api::options::ThinkingConfig;
use crate::llm::capabilities::{self, WireDialect};

/// A validated LLM dispatch route: the provider/model pair plus the resolved
/// wire dialect. Constructed only via [`Route::resolve`], which rejects the
/// thinking-over-lossy-wire pairing rather than letting it be dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub provider: String,
    pub model: String,
    pub dialect: WireDialect,
}

/// Why a `(provider, model, thinking)` triple could not resolve to a route that
/// can actually serve the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteError {
    /// A thinking-enabled Anthropic-family model resolved to a wire dialect that
    /// cannot surface Claude's extended-thinking stream (the OpenAI-compatible
    /// transport). Serving this pairing would bill the thinking budget and
    /// return no content.
    ReasoningNotSurfaced {
        provider: String,
        model: String,
        dialect: WireDialect,
    },
}

impl RouteError {
    /// A user-facing, actionable message. Names the exact route and points at
    /// the usual cause (a dropped/mis-scoped provider) so the upstream bug gets
    /// fixed rather than silently worked around.
    pub(crate) fn into_message(self) -> String {
        match self {
            RouteError::ReasoningNotSurfaced {
                provider,
                model,
                dialect,
            } => format!(
                "route rejected before dispatch: thinking-enabled Anthropic-family model \
                 {model} resolved to the {} wire dialect via provider \"{provider}\", which \
                 cannot surface extended thinking (it would bill the thinking budget and return \
                 an empty completion). A correctly-scoped Claude route resolves to the anthropic \
                 (native Messages) dialect; this usually means the provider was dropped or \
                 mis-scoped upstream (e.g. an escalation that kept the cheap model's provider). \
                 Set provider = \"anthropic\" (or another native Claude route), or disable \
                 thinking for this call.",
                dialect.as_str()
            ),
        }
    }
}

/// Anthropic/Claude-family detection, mirroring the catalog's existing family
/// signal (`capabilities::suggested_native_tools`, which keys on
/// `model_id.contains("claude")`). Keyed on the model id because the provider
/// string is exactly what gets dropped on the failing escalation path.
fn is_anthropic_family_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

impl Route {
    /// Resolve `(provider, model)` against the capability catalog, given the
    /// request's thinking configuration. Returns [`RouteError`] for a pairing
    /// that would be billed-but-empty; otherwise a [`Route`] whose dispatch is
    /// byte-identical to the pre-guard behavior.
    pub(crate) fn resolve(
        provider: &str,
        model: &str,
        thinking: &ThinkingConfig,
    ) -> Result<Route, RouteError> {
        let dialect = capabilities::lookup(provider, model).message_wire_format;

        // The only lossy pairing in practice: a thinking-enabled Claude model
        // over the OpenAI-compatible wire. Gate on `thinking.is_enabled()` so
        // legitimate non-thinking compat calls to a Claude rehost are untouched,
        // and on the Anthropic family so legitimate compat reasoning models
        // (qwen/deepseek/gpt-oss, which DO stream `reasoning_content`) are never
        // false-flagged.
        if thinking.is_enabled()
            && dialect == WireDialect::OpenAiCompat
            && is_anthropic_family_model(model)
        {
            return Err(RouteError::ReasoningNotSurfaced {
                provider: provider.to_string(),
                model: model.to_string(),
                dialect,
            });
        }

        Ok(Route {
            provider: provider.to_string(),
            model: model.to_string(),
            dialect,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> ThinkingConfig {
        ThinkingConfig::Enabled {
            budget_tokens: None,
        }
    }

    #[test]
    fn dropped_provider_claude_with_thinking_is_rejected() {
        // The exact harn#3956 mechanism: escalation kept the cheap model's
        // provider ("fireworks") but swapped in a Claude model. No Anthropic
        // rule matches that provider scope, so the dialect defaults to the
        // OpenAI-compatible transport that drops thinking.
        let err = Route::resolve("fireworks", "claude-sonnet-4-6", &enabled())
            .expect_err("thinking-enabled claude over a non-anthropic provider must be rejected");
        assert!(matches!(
            err,
            RouteError::ReasoningNotSurfaced {
                dialect: WireDialect::OpenAiCompat,
                ..
            }
        ));
        assert!(err.into_message().contains("claude-sonnet-4-6"));
    }

    #[test]
    fn native_anthropic_claude_with_thinking_resolves() {
        // A correctly-scoped Claude route resolves to the native Messages
        // dialect and is served byte-identically to before.
        let route = Route::resolve("anthropic", "claude-sonnet-4-6", &enabled())
            .expect("native anthropic route must resolve");
        assert_eq!(route.dialect, WireDialect::Anthropic);
    }

    #[test]
    fn dropped_provider_claude_without_thinking_resolves() {
        // With thinking disabled there is nothing to lose over the compat wire,
        // so this must NOT error (e.g. a non-thinking call to a Claude rehost).
        let route = Route::resolve("fireworks", "claude-sonnet-4-6", &ThinkingConfig::Disabled)
            .expect("non-thinking claude-over-compat must resolve");
        assert_eq!(route.dialect, WireDialect::OpenAiCompat);
    }

    #[test]
    fn non_anthropic_reasoning_model_over_compat_is_not_flagged() {
        // Legitimate compat reasoning models stream `reasoning_content` and must
        // never be false-flagged, even with thinking enabled.
        let route = Route::resolve("openrouter", "qwen/qwen3.6-35b-a3b", &enabled())
            .expect("non-anthropic compat reasoning model must resolve");
        assert_eq!(route.dialect, WireDialect::OpenAiCompat);
    }

    #[test]
    fn mock_claude_with_thinking_resolves_native() {
        // `mock` spoofs the Anthropic capability row for Claude-shape ids, so
        // mock-based thinking tests keep resolving (no false rejection).
        let route = Route::resolve("mock", "claude-opus-4-7", &enabled())
            .expect("mock claude must resolve to the anthropic dialect");
        assert_eq!(route.dialect, WireDialect::Anthropic);
    }
}
