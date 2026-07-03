//! ResolvedDispatch — one self-contained record of what an LLM call actually
//! dispatched, and what came back.
//!
//! WHY THIS EXISTS
//!
//! Answering "what provider/model/wire-format/thinking did this LLM call
//! actually use, where did each of those come from, and what did it return?"
//! used to require joining the `provider_call_request` and
//! `provider_call_response` transcript events by `call_id`, cross-referencing
//! `capabilities.toml` to learn the wire format, and reconstructing the
//! provenance of each field by reading five-plus scattered resolution layers.
//! A real 2026-07 escalation incident took six log-greps and multiple
//! contradictory root-cause passes (transport-routing -> provider-drop ->
//! refuted -> mislabeled error string) precisely because no single record
//! captured the *final resolved decision*.
//!
//! `resolved_dispatch` collapses that into ONE append-only transcript event
//! per LLM call. It is:
//!   - self-contained (no join needed),
//!   - deterministic in its wire-format/base-url fields (derived from the
//!     capability registry, the single source of truth), and
//!   - provenance-bearing: each of provider/model/wire_format/thinking/
//!     tool_format records WHERE it came from. The smoking-gun value is
//!     `inherited_from_primary` — it would have made the escalation incident
//!     obvious on the first read.
//!
//! This module is observability-only. It reads the request options + the
//! result and emits a record; it never feeds back into request construction,
//! so the model's next-turn payload is byte-identical with or without it.

use super::api::{LlmCallOptions, ThinkingConfig};

/// Where a single resolved dispatch field came from. Carried on
/// [`LlmCallOptions::dispatch_provenance`], populated by the pipeline resolver
/// (Burin's smart-escalation / model-selection layers, threaded through the
/// agent-loop options). `Unknown` is the default when no resolver annotated the
/// call — e.g. a raw `llm_call(...)` from script context.
///
/// The string values are a small, stable vocabulary so downstream tooling
/// (the harness-debugger `dispatch_trace` MCP tool) can filter on them:
///   - `operator_pin`         — an explicit operator/env pin (e.g.
///                              `BURIN_EVAL_SMART_PROVIDER`).
///   - `pipeline_input`       — a `selected_*` field on the pipeline input.
///   - `escalation_override`  — chosen by the smart-escalation resolver.
///   - `catalog_default`      — filled from the provider catalog / capability
///                              registry default.
///   - `inherited_from_primary` — no pin/override existed, so the value fell
///                              through from the cheap primary model. THIS is
///                              the value that flags a silent-inheritance bug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchProvenance {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub wire_format: Option<String>,
    pub thinking: Option<String>,
    pub tool_format: Option<String>,
}

impl DispatchProvenance {
    /// Canonical provenance origin for a value that fell through from the
    /// primary model with no pin or override. Named as a constant so the
    /// resolver and the tests reference the same smoking-gun literal.
    pub const INHERITED_FROM_PRIMARY: &'static str = "inherited_from_primary";
    pub const OPERATOR_PIN: &'static str = "operator_pin";
    pub const ESCALATION_OVERRIDE: &'static str = "escalation_override";
    pub const PIPELINE_INPUT: &'static str = "pipeline_input";
    pub const CATALOG_DEFAULT: &'static str = "catalog_default";

    /// Parse a `dispatch_provenance` option dict supplied by the pipeline
    /// resolver into the typed provenance. Each entry is a per-field origin
    /// string (`operator_pin`, `escalation_override`, `inherited_from_primary`,
    /// ...); absent entries stay `None` and surface as `"unknown"` in the
    /// record. Returns `None` when the value is absent or not a dict, so a
    /// non-annotating caller pays nothing.
    pub fn from_vm_value(value: &crate::value::VmValue) -> Option<Self> {
        let dict = value.as_dict()?;
        let field = |key: &str| -> Option<String> {
            dict.get(key)
                .map(|v| v.as_str_cow().into_owned())
                .filter(|s| !s.is_empty())
        };
        Some(Self {
            provider: field("provider"),
            model: field("model"),
            wire_format: field("wire_format"),
            thinking: field("thinking"),
            tool_format: field("tool_format"),
        })
    }

    fn origin_or_unknown(value: &Option<String>) -> &str {
        value.as_deref().unwrap_or("unknown")
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": Self::origin_or_unknown(&self.provider),
            "model": Self::origin_or_unknown(&self.model),
            "wire_format": Self::origin_or_unknown(&self.wire_format),
            "thinking": Self::origin_or_unknown(&self.thinking),
            "tool_format": Self::origin_or_unknown(&self.tool_format),
        })
    }
}

/// The normalized outcome of a dispatched LLM call. Derived from the result (on
/// success) or the thrown error (on failure) so a consumer never has to
/// pattern-match raw error strings.
///
/// The `served` vs `empty_completion_transient_recovered` vs
/// `empty_completion_terminal` split is the exact distinction the escalation
/// guard hinges on (mirrors burin-code#3550's `escalation_served_empty_signal`
/// vocabulary — an empty flake the runtime RETRIED and recovered from is not a
/// dead lane; only a TERMINAL unrecovered empty is). The whole 2026-07 incident
/// was a series of transient recovered empties misread as a terminal serve
/// failure; this enum makes the two unmistakable in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The call returned committed content / a tool call / thinking on the
    /// first attempt.
    Served {
        completion_tokens: i64,
        content_len: usize,
    },
    /// The provider emitted one or more empty completions that the runtime
    /// retried and RECOVERED from — the call ultimately served. Not a dead
    /// lane; the retry machinery did its job.
    EmptyCompletionTransientRecovered {
        completion_tokens: i64,
        content_len: usize,
        empty_retries: usize,
    },
    /// The provider billed output tokens but committed nothing, and the runtime
    /// exhausted its retry budget (or surfaced the empty as a terminal error).
    /// THIS is the "escalation served empty" dead-lane signal.
    EmptyCompletionTerminal {
        completion_tokens: i64,
    },
    /// The provider hit a usage / quota / rate limit.
    UsageLimit,
    /// Any other provider-side error, with a short class label.
    ProviderError {
        class: String,
    },
}

impl DispatchOutcome {
    /// Classify a successful [`super::api::LlmResult`], given how many
    /// empty-completion retries preceded this (recovered) result. An empty
    /// committed message with billed output that survives to this point is
    /// TERMINAL (the retry budget was exhausted and the loop is returning the
    /// empty result unchanged); a served result after >0 empty retries is a
    /// transient-recovered flake; a clean first-attempt serve is `served`.
    pub fn from_result(result: &super::api::LlmResult, empty_retries: usize) -> Self {
        let content_len = result.text.len();
        let committed_nothing = result.text.is_empty()
            && result.tool_calls.is_empty()
            && result.thinking.as_deref().map(str::is_empty).unwrap_or(true);
        if committed_nothing && result.output_tokens > 0 {
            return DispatchOutcome::EmptyCompletionTerminal {
                completion_tokens: result.output_tokens,
            };
        }
        if empty_retries > 0 {
            return DispatchOutcome::EmptyCompletionTransientRecovered {
                completion_tokens: result.output_tokens,
                content_len,
                empty_retries,
            };
        }
        DispatchOutcome::Served {
            completion_tokens: result.output_tokens,
            content_len,
        }
    }

    /// Classify a thrown TERMINAL error message into the outcome vocabulary.
    /// Only called on the surfaced (non-retryable) error, so an empty-completion
    /// error here is by definition terminal. Keyed on the same stable substrings
    /// the retry/skip classifiers use, so the record agrees with the runtime's
    /// own routing decisions.
    pub fn from_error_message(message: &str) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("completion_tokens=")
            && (lower.contains("delivered no content")
                || (lower.contains("no dispatchable tool call or answer")
                    && lower.contains("upstream contract violation")))
        {
            // The token count is embedded in the message but is not needed for
            // the class; downstream consumers read `completion_tokens` from the
            // sibling `provider_call_response` when they need the exact value.
            // A surfaced empty-completion error is terminal by construction.
            return DispatchOutcome::EmptyCompletionTerminal {
                completion_tokens: 0,
            };
        }
        if lower.contains("rate limit")
            || lower.contains("quota")
            || lower.contains("usage limit")
            || lower.contains("429")
        {
            return DispatchOutcome::UsageLimit;
        }
        DispatchOutcome::ProviderError {
            class: provider_error_class(&lower),
        }
    }

    /// Stable machine label. `dispatch_trace` filters on these.
    pub fn label(&self) -> &'static str {
        match self {
            DispatchOutcome::Served { .. } => "served",
            DispatchOutcome::EmptyCompletionTransientRecovered { .. } => {
                "empty_completion_transient_recovered"
            }
            DispatchOutcome::EmptyCompletionTerminal { .. } => "empty_completion_terminal",
            DispatchOutcome::UsageLimit => "usage_limit",
            DispatchOutcome::ProviderError { .. } => "provider_error",
        }
    }

    /// True only for the dead-lane signal: a terminal, unrecovered empty
    /// completion. Transient-recovered empties are explicitly NOT this.
    pub fn is_served_empty(&self) -> bool {
        matches!(self, DispatchOutcome::EmptyCompletionTerminal { .. })
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            DispatchOutcome::Served {
                completion_tokens,
                content_len,
            } => serde_json::json!({
                "kind": "served",
                "completion_tokens": completion_tokens,
                "content_len": content_len,
            }),
            DispatchOutcome::EmptyCompletionTransientRecovered {
                completion_tokens,
                content_len,
                empty_retries,
            } => serde_json::json!({
                "kind": "empty_completion_transient_recovered",
                "completion_tokens": completion_tokens,
                "content_len": content_len,
                "empty_retries": empty_retries,
            }),
            DispatchOutcome::EmptyCompletionTerminal { completion_tokens } => serde_json::json!({
                "kind": "empty_completion_terminal",
                "completion_tokens": completion_tokens,
                "content_len": 0,
            }),
            DispatchOutcome::UsageLimit => serde_json::json!({
                "kind": "usage_limit",
            }),
            DispatchOutcome::ProviderError { class } => serde_json::json!({
                "kind": "provider_error",
                "class": class,
            }),
        }
    }
}

/// Coarse class label for a provider error message so `dispatch_trace` can
/// bucket failures without exposing the full (potentially secret-bearing) text.
fn provider_error_class(lower: &str) -> String {
    for (needle, class) in [
        ("api error", "api_error"),
        ("timed out", "timeout"),
        ("timeout", "timeout"),
        ("connection", "connection"),
        ("missing content array", "malformed_response"),
        ("authentication", "auth"),
        ("unauthorized", "auth"),
        ("401", "auth"),
        ("not found", "not_found"),
        ("404", "not_found"),
        ("overloaded", "overloaded"),
        ("500", "server_error"),
        ("502", "server_error"),
        ("503", "server_error"),
    ] {
        if lower.contains(needle) {
            return class.to_string();
        }
    }
    "unknown".to_string()
}

/// The wire format an LLM call dispatched over, derived from the capability
/// registry (the single source of truth). This is the field whose absence made
/// the escalation incident hard to root-cause.
pub fn wire_format_for(provider: &str, model: &str) -> &'static str {
    if super::capabilities::lookup(provider, model).message_wire_format == "anthropic" {
        "anthropic_native"
    } else {
        "openai_compat"
    }
}

/// The host of the base URL the call went to (e.g. `api.anthropic.com`), so a
/// misrouted call to a proxy / third-party rehost is visible at a glance. Falls
/// back to the raw base URL when it has no parseable host.
fn base_url_host(provider: &str) -> String {
    let base_url = super::helpers::ResolvedProvider::resolve(provider).base_url;
    base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(str::to_string)
        .unwrap_or(base_url)
}

fn thinking_json(thinking: &ThinkingConfig) -> serde_json::Value {
    match thinking {
        ThinkingConfig::Disabled => serde_json::json!({"mode": "off", "enabled": false}),
        ThinkingConfig::Enabled { budget_tokens } => serde_json::json!({
            "mode": "enabled",
            "enabled": true,
            "budget_tokens": budget_tokens,
        }),
        ThinkingConfig::Adaptive => serde_json::json!({"mode": "adaptive", "enabled": true}),
        ThinkingConfig::Effort { level } => serde_json::json!({
            "mode": "effort",
            "level": level.as_str(),
            "enabled": !thinking.is_disabled(),
        }),
    }
}

/// Build the single self-contained `resolved_dispatch` transcript record for
/// one LLM call. `iteration`/`call_id`/`span_id` correlate it with the sibling
/// `provider_call_request` / `provider_call_response` events; the record itself
/// carries everything a consumer needs to answer "what dispatched, from where,
/// and what came back" without any join.
pub fn build_record(
    iteration: usize,
    call_id: &str,
    span_id: Option<u64>,
    timestamp: String,
    opts: &LlmCallOptions,
    effective_tool_format: &str,
    outcome: &DispatchOutcome,
) -> serde_json::Value {
    let provenance = opts.dispatch_provenance.clone().unwrap_or_default();
    serde_json::json!({
        "type": "resolved_dispatch",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": span_id,
        "timestamp": timestamp,
        "provider": opts.provider,
        "model": opts.model,
        "wire_format": wire_format_for(&opts.provider, &opts.model),
        "thinking": thinking_json(&opts.thinking),
        "tool_format": effective_tool_format,
        "base_url_host": base_url_host(&opts.provider),
        "provenance": provenance.to_json(),
        "outcome": outcome.to_json(),
        "outcome_kind": outcome.label(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_native_for_anthropic_claude() {
        // A claude-* model on the anthropic provider must resolve native.
        assert_eq!(
            wire_format_for("anthropic", "claude-sonnet-4-6"),
            "anthropic_native"
        );
    }

    #[test]
    fn wire_format_compat_for_openai_style() {
        assert_eq!(wire_format_for("openai", "gpt-4o"), "openai_compat");
    }

    #[test]
    fn outcome_empty_completion_terminal_from_billed_no_content() {
        // The exact string the FIXED transport throw now emits — note it names
        // the native wire style, not the old hardcoded "openai-compatible".
        let msg = "anthropic-native model anthropic:claude-sonnet-4-6 reported \
                   completion_tokens=8 but delivered no content, reasoning, or tool calls";
        assert_eq!(
            DispatchOutcome::from_error_message(msg),
            DispatchOutcome::EmptyCompletionTerminal {
                completion_tokens: 0
            }
        );
        assert!(DispatchOutcome::from_error_message(msg).is_served_empty());
    }

    #[test]
    fn transient_recovered_is_not_served_empty() {
        // A recovered flake (>0 empty retries but the call ultimately served)
        // must NOT be flagged as the dead-lane signal. This is the exact
        // misclassification the 2026-07 incident hinged on.
        let recovered = DispatchOutcome::EmptyCompletionTransientRecovered {
            completion_tokens: 487,
            content_len: 1666,
            empty_retries: 3,
        };
        assert_eq!(recovered.label(), "empty_completion_transient_recovered");
        assert!(!recovered.is_served_empty());
    }

    #[test]
    fn outcome_usage_limit_from_quota() {
        assert_eq!(
            DispatchOutcome::from_error_message("provider returned 429 rate limit exceeded"),
            DispatchOutcome::UsageLimit
        );
    }

    #[test]
    fn outcome_provider_error_class() {
        match DispatchOutcome::from_error_message("anthropic API error: overloaded") {
            DispatchOutcome::ProviderError { class } => assert_eq!(class, "api_error"),
            other => panic!("expected provider_error, got {other:?}"),
        }
    }

    #[test]
    fn provenance_inherited_marker_is_stable() {
        assert_eq!(
            DispatchProvenance::INHERITED_FROM_PRIMARY,
            "inherited_from_primary"
        );
        let prov = DispatchProvenance {
            provider: Some(DispatchProvenance::INHERITED_FROM_PRIMARY.to_string()),
            ..Default::default()
        };
        let json = prov.to_json();
        assert_eq!(json["provider"], "inherited_from_primary");
        // Unset fields serialize as the explicit "unknown" sentinel so a
        // consumer never sees a missing key.
        assert_eq!(json["model"], "unknown");
    }
}
