//! Per-call reasoning receipt — what reasoning directive actually left for the
//! provider, read out of the request body that was built.
//!
//! WHY THIS EXISTS
//!
//! A reasoning effort level is lossy at the wire, independently at every
//! dialect, and the loss is invisible in the response:
//!
//!   * the Gemini Interactions ladder collapses `high`, `xhigh` and `max` to
//!     one rung, and has no "off";
//!   * `generateContent` maps those same three levels onto one thinking
//!     budget;
//!   * Anthropic emits the effort field only when the model row admits it, and
//!     otherwise sends adaptive thinking with no level at all;
//!   * two OpenAI-compatible reasoning dialects reduce every level to an
//!     on/off flag, and one omits the field entirely above its two states.
//!
//! Cost and latency move with the level, so a comparison that silently
//! compares two different rungs reports a false result. The resolved
//! [`ThinkingConfig`] already appears in the LLM transcript, but the transcript
//! is written only when a transcript directory is configured, and the resolved
//! value is not the sent value.
//!
//! WHAT THIS RECORDS
//!
//! One receipt per LLM call, written at
//! [`crate::llm::api::DialectContract::build_request_body`], which every
//! dialect-lowered route passes through. Two adapters build their own bodies
//! and never reach that contract — the OpenAI Responses API and Bedrock
//! Converse — so each records its own receipt at its builder. `sent` is read
//! back out of the body that was just built, at the field path the dialect
//! declares, so it is the bytes that go to the provider rather than a second
//! copy of the dialect table that could drift from it.
//!
//! KNOWN BLIND SPOT
//!
//! The receipt is taken where the body is lowered. A caller that injects a
//! reasoning field through `provider_overrides` after lowering changes the
//! wire without changing the receipt. Bedrock Converse, whose only reasoning
//! field arrives that way, takes its receipt after overrides are applied; the
//! dialect-lowered routes do not.
//!
//! ABSENCE IS NOT AGREEMENT
//!
//! Three sent states are distinct, and none of them is "the level was
//! carried":
//!
//!   * `carried` — the dialect's reasoning field is present, with its value;
//!   * `omitted` — the dialect declares reasoning fields and emitted none;
//!   * `unreported` — the dialect declares no reasoning field path, so this
//!     module cannot speak for the call.
//!
//! An empty receipt list on a run that made LLM calls is likewise a gap, not
//! agreement: the execution evidence envelope carries `None` for a producer
//! that never reported and `Some([])` for a run that made no calls.
//!
//! This module is observability-only. It reads the built body and never feeds
//! back into it, so the request bytes are identical with and without it.

use std::cell::RefCell;

/// The most receipts one execution retains. A run that exceeds this keeps the
/// earliest receipts and reports the overflow as an evidence gap rather than
/// silently presenting a partial list as complete.
pub const MAX_REASONING_RECEIPTS: usize = 1024;

/// Sent-state vocabulary. Stable strings: consumers filter on them.
pub const SENT_CARRIED: &str = "carried";
pub const SENT_OMITTED: &str = "omitted";
pub const SENT_UNREPORTED: &str = "unreported";

/// What one LLM call resolved to, and what its dialect actually sent.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ReasoningReceipt {
    /// Zero-based position of this call within the execution.
    pub index: u32,
    pub provider: String,
    pub model: String,
    /// Wire dialect that lowered the request, for example `anthropic` or
    /// `gemini_interactions`.
    pub wire_dialect: String,
    /// Resolved mode: `disabled`, `enabled`, `adaptive`, or `effort`.
    pub resolved_mode: String,
    /// Resolved effort rung, present only for `effort`.
    pub resolved_level: Option<String>,
    /// Resolved explicit thinking budget, present only for `enabled` with one.
    pub resolved_budget_tokens: Option<u32>,
    /// One of [`SENT_CARRIED`], [`SENT_OMITTED`], [`SENT_UNREPORTED`].
    pub sent_status: String,
    /// Dotted field path the value was read from, when one carried a value.
    pub sent_field: Option<String>,
    /// The verbatim value found at `sent_field`.
    pub sent_value: Option<serde_json::Value>,
    /// Every reasoning-bearing field path this dialect declares. Present on
    /// `omitted` so a reader can see what was looked for and not found.
    pub candidate_fields: Vec<String>,
}

impl ReasoningReceipt {
    /// True when the resolved rung is an effort level that the sent value does
    /// not state. This is the mislabel signal: the call asked for a rung and
    /// the wire carries something else, or nothing.
    pub fn level_lost_at_the_wire(&self) -> bool {
        let Some(level) = self.resolved_level.as_deref() else {
            return false;
        };
        match self.sent_status.as_str() {
            SENT_CARRIED => !self
                .sent_value
                .as_ref()
                .is_some_and(|value| value_states_level(value, level)),
            _ => true,
        }
    }
}

/// Whether a sent value states this exact rung anywhere inside it. The value
/// shapes differ by dialect (`"high"`, `{"effort": "high"}`,
/// `{"thinkingBudget": 8192}`), so the check is on the rendered strings the
/// dialects use, never on a re-derivation of the mapping.
fn value_states_level(value: &serde_json::Value, level: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text == level,
        serde_json::Value::Object(map) => map.values().any(|v| value_states_level(v, level)),
        serde_json::Value::Array(items) => items.iter().any(|v| value_states_level(v, level)),
        _ => false,
    }
}

thread_local! {
    static RECEIPTS: RefCell<Vec<ReasoningReceipt>> = const { RefCell::new(Vec::new()) };
    static DROPPED: RefCell<u32> = const { RefCell::new(0) };
}

/// Clear the collector at a top-level execution boundary so one run's receipts
/// can never be persisted onto the next run's record.
pub fn reset_reasoning_receipts() {
    RECEIPTS.with(|slot| slot.borrow_mut().clear());
    DROPPED.with(|slot| *slot.borrow_mut() = 0);
}

/// Receipts recorded so far, without consuming them.
pub fn peek_reasoning_receipts() -> Vec<ReasoningReceipt> {
    RECEIPTS.with(|slot| slot.borrow().clone())
}

/// How many receipts were discarded after [`MAX_REASONING_RECEIPTS`].
pub fn dropped_reasoning_receipts() -> u32 {
    DROPPED.with(|slot| *slot.borrow())
}

/// Record one receipt for a request that has just been lowered.
///
/// `candidate_fields` is the dialect's declared reasoning field paths, in the
/// order they are searched; an empty slice means the dialect does not declare
/// any and the receipt reports `unreported`.
pub(crate) fn record(
    provider: &str,
    model: &str,
    wire_dialect: &str,
    thinking: &crate::llm::api::ThinkingConfig,
    candidate_fields: &[&str],
    body: &serde_json::Value,
) {
    let (resolved_mode, resolved_level, resolved_budget_tokens) = describe(thinking);
    let found = candidate_fields
        .iter()
        .find_map(|path| lookup(body, path).map(|value| ((*path).to_string(), value.clone())));
    let (sent_status, sent_field, sent_value) = match (candidate_fields.is_empty(), found) {
        (true, _) => (SENT_UNREPORTED, None, None),
        (false, Some((path, value))) => (SENT_CARRIED, Some(path), Some(value)),
        (false, None) => (SENT_OMITTED, None, None),
    };
    RECEIPTS.with(|slot| {
        let mut receipts = slot.borrow_mut();
        if receipts.len() >= MAX_REASONING_RECEIPTS {
            DROPPED.with(|dropped| {
                let mut dropped = dropped.borrow_mut();
                *dropped = dropped.saturating_add(1);
            });
            return;
        }
        let index = u32::try_from(receipts.len()).unwrap_or(u32::MAX);
        receipts.push(ReasoningReceipt {
            index,
            provider: provider.to_string(),
            model: model.to_string(),
            wire_dialect: wire_dialect.to_string(),
            resolved_mode: resolved_mode.to_string(),
            resolved_level,
            resolved_budget_tokens,
            sent_status: sent_status.to_string(),
            sent_field,
            sent_value,
            candidate_fields: candidate_fields
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
        });
    });
}

fn describe(
    thinking: &crate::llm::api::ThinkingConfig,
) -> (&'static str, Option<String>, Option<u32>) {
    use crate::llm::api::ThinkingConfig;
    match thinking {
        ThinkingConfig::Disabled => ("disabled", None, None),
        ThinkingConfig::Enabled { budget_tokens } => ("enabled", None, *budget_tokens),
        ThinkingConfig::Adaptive => ("adaptive", None, None),
        ThinkingConfig::Effort { level } => ("effort", Some(level.as_str().to_string()), None),
    }
}

/// Read a dotted path out of a JSON body. Dialect field paths are literal
/// object keys; no array or wildcard syntax is accepted, so a path that does
/// not resolve is a genuine absence rather than an unsupported expression.
fn lookup<'a>(body: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(body, |value, segment| value.get(segment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{ReasoningEffort, ThinkingConfig};

    struct Isolated;

    impl Isolated {
        fn enter() -> Self {
            reset_reasoning_receipts();
            Self
        }
    }

    impl Drop for Isolated {
        fn drop(&mut self) {
            reset_reasoning_receipts();
        }
    }

    #[test]
    fn a_dialect_that_carries_the_rung_agrees_with_the_resolved_level() {
        let _isolated = Isolated::enter();
        record(
            "openrouter",
            "some/model",
            "openai_compat",
            &ThinkingConfig::Effort {
                level: ReasoningEffort::XHigh,
            },
            &["reasoning", "reasoning_effort"],
            &serde_json::json!({"reasoning": {"effort": "xhigh"}}),
        );
        let receipt = &peek_reasoning_receipts()[0];
        assert_eq!(receipt.sent_status, SENT_CARRIED);
        assert_eq!(receipt.sent_field.as_deref(), Some("reasoning"));
        assert!(!receipt.level_lost_at_the_wire());
    }

    #[test]
    fn a_dialect_that_lowers_the_rung_reports_the_value_it_sent() {
        let _isolated = Isolated::enter();
        record(
            "gemini",
            "some-model",
            "gemini_interactions",
            &ThinkingConfig::Effort {
                level: ReasoningEffort::XHigh,
            },
            &["generation_config.thinking_level"],
            &serde_json::json!({"generation_config": {"thinking_level": "high"}}),
        );
        let receipt = &peek_reasoning_receipts()[0];
        assert_eq!(receipt.sent_status, SENT_CARRIED);
        assert_eq!(receipt.sent_value, Some(serde_json::json!("high")));
        assert_eq!(receipt.resolved_level.as_deref(), Some("xhigh"));
        assert!(
            receipt.level_lost_at_the_wire(),
            "xhigh lowered to high must read as a loss"
        );
    }

    #[test]
    fn a_declared_field_that_never_appeared_is_omitted_not_agreement() {
        let _isolated = Isolated::enter();
        record(
            "groq",
            "some-model",
            "openai_compat",
            &ThinkingConfig::Effort {
                level: ReasoningEffort::High,
            },
            &["reasoning", "reasoning_effort"],
            &serde_json::json!({"model": "some-model"}),
        );
        let receipt = &peek_reasoning_receipts()[0];
        assert_eq!(receipt.sent_status, SENT_OMITTED);
        assert_eq!(receipt.sent_value, None);
        assert_eq!(
            receipt.candidate_fields,
            vec!["reasoning", "reasoning_effort"]
        );
        assert!(receipt.level_lost_at_the_wire());
    }

    #[test]
    fn a_dialect_with_no_declared_path_says_unreported_rather_than_omitted() {
        let _isolated = Isolated::enter();
        record(
            "custom",
            "some-model",
            "custom_wire",
            &ThinkingConfig::Adaptive,
            &[],
            &serde_json::json!({}),
        );
        let receipt = &peek_reasoning_receipts()[0];
        assert_eq!(receipt.sent_status, SENT_UNREPORTED);
        assert!(receipt.candidate_fields.is_empty());
    }

    #[test]
    fn a_call_with_no_effort_rung_never_reads_as_a_lost_level() {
        let _isolated = Isolated::enter();
        record(
            "anthropic",
            "some-model",
            "anthropic",
            &ThinkingConfig::Enabled {
                budget_tokens: Some(10_000),
            },
            &["thinking", "output_config.effort"],
            &serde_json::json!({"thinking": {"type": "enabled", "budget_tokens": 10000}}),
        );
        let receipt = &peek_reasoning_receipts()[0];
        assert_eq!(receipt.resolved_budget_tokens, Some(10_000));
        assert!(!receipt.level_lost_at_the_wire());
    }

    #[test]
    fn a_reset_clears_one_executions_receipts_from_the_next() {
        let _isolated = Isolated::enter();
        record(
            "anthropic",
            "some-model",
            "anthropic",
            &ThinkingConfig::Adaptive,
            &["thinking"],
            &serde_json::json!({"thinking": {"type": "adaptive"}}),
        );
        assert_eq!(peek_reasoning_receipts().len(), 1);
        reset_reasoning_receipts();
        assert!(peek_reasoning_receipts().is_empty());
    }

    #[test]
    fn overflow_is_counted_rather_than_silently_truncating() {
        let _isolated = Isolated::enter();
        for _ in 0..(MAX_REASONING_RECEIPTS + 3) {
            record(
                "anthropic",
                "some-model",
                "anthropic",
                &ThinkingConfig::Adaptive,
                &["thinking"],
                &serde_json::json!({"thinking": {"type": "adaptive"}}),
            );
        }
        assert_eq!(peek_reasoning_receipts().len(), MAX_REASONING_RECEIPTS);
        assert_eq!(dropped_reasoning_receipts(), 3);
    }

    #[test]
    fn a_dotted_path_reads_only_literal_object_keys() {
        let body =
            serde_json::json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": 8192}}});
        assert_eq!(
            lookup(&body, "generationConfig.thinkingConfig"),
            Some(&serde_json::json!({"thinkingBudget": 8192}))
        );
        assert_eq!(lookup(&body, "generationConfig.absent"), None);
    }
}
