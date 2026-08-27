//! One vocabulary for reading facts out of a persisted session event.
//!
//! Session events are stored as an opaque JSON payload wrapping a
//! `transcript_event` envelope, so every consumer that wants a typed fact —
//! which model a call used, whether a tool succeeded, what the loop's terminal
//! state was — has to know the same JSON pointers. Two consumers already do
//! ([`crate::session_timeline`] and the run-record projector), and issue #6118
//! was caused by exactly this shape of drift: an emitter and a reader agreed by
//! convention until they quietly stopped agreeing.
//!
//! So the pointers live here once, as named constants, with typed accessors
//! over them. A reader that needs a new fact adds it here rather than
//! hardcoding a pointer at its own call site.

use serde_json::Value;

/// Pointer to the terminal loop status recorded on an `agent_run_terminal`
/// event, e.g. `"done"`. [`crate::llm::agent_terminal_class`] owns what the
/// values mean.
pub(crate) const FINAL_STATUS: &str = "/transcript_event/metadata/final_status";
/// Why the loop stopped, e.g. `"pace_cutoff"`. Present alongside
/// [`FINAL_STATUS`] and orthogonal to it: a run can stop for a budget reason
/// and still report a successful final status.
pub(crate) const STOP_REASON: &str = "/transcript_event/metadata/stop_reason";
/// Terminal error text, when the loop ended on one.
pub(crate) const TERMINAL_ERROR: &str = "/transcript_event/metadata/error";
/// Coarse terminal classification assigned by
/// [`crate::llm::agent_terminal_class`].
pub(crate) const TERMINAL_CLASS: &str = "/transcript_event/metadata/terminal_class";
/// Producer-owned terminal kind recorded by `agent_session_finalize`.
pub(crate) const TERMINAL_KIND: &str = "/transcript_event/metadata/terminal/kind";
/// Producer-owned terminal attribution paired with [`TERMINAL_KIND`].
pub(crate) const TERMINAL_OWNER: &str = "/transcript_event/metadata/terminal/owner";
/// Producer-owned explanation paired with [`TERMINAL_KIND`]. This can be more
/// precise than the legacy loop-level [`STOP_REASON`].
pub(crate) const TERMINAL_REASON: &str = "/transcript_event/metadata/terminal/reason";

/// Provider-assigned name of the model that served an `llm_call`.
pub(crate) const MODEL: &str = "/transcript_event/metadata/model";
/// Provider that served an `llm_call`, e.g. `"openai"`.
pub(crate) const PROVIDER: &str = "/transcript_event/metadata/provider";
pub(crate) const INPUT_TOKENS: &str = "/transcript_event/metadata/input_tokens";
pub(crate) const OUTPUT_TOKENS: &str = "/transcript_event/metadata/output_tokens";
pub(crate) const CACHE_READ_TOKENS: &str = "/transcript_event/metadata/cache_read_tokens";
pub(crate) const CACHE_WRITE_TOKENS: &str = "/transcript_event/metadata/cache_write_tokens";
pub(crate) const COST_USD: &str = "/transcript_event/metadata/cost_usd";
/// Whether provider usage facts were reported or remain unknown.
pub(crate) const ACCOUNTING_STATUS: &str = "/transcript_event/metadata/accounting_status";
/// Provider requests issued for one logical `llm_call`, including the one that
/// succeeded. Absent on calls recorded before the field existed, which is
/// distinct from a recorded 1.
pub(crate) const PROVIDER_ATTEMPTS_TOTAL: &str =
    "/transcript_event/metadata/provider_attempts/total";
/// Requests rejected with a retryable rate-limit error before one succeeded.
pub(crate) const PROVIDER_ATTEMPTS_RATE_LIMITED: &str =
    "/transcript_event/metadata/provider_attempts/rate_limited";
/// Requests that returned a completion with nothing the loop could act on.
pub(crate) const PROVIDER_ATTEMPTS_EMPTY: &str =
    "/transcript_event/metadata/provider_attempts/empty_completion";
/// Retryable failures that were neither rate limiting nor empty completions.
pub(crate) const PROVIDER_ATTEMPTS_OTHER: &str =
    "/transcript_event/metadata/provider_attempts/other";

/// Provider-stable identifier joining a `tool_call` to its `tool_call_update`
/// and `tool_result` events.
pub(crate) const TOOL_CALL_ID: &str = "/transcript_event/metadata/tool_call_id";
pub(crate) const TOOL_NAME: &str = "/transcript_event/metadata/tool_name";
/// Arguments as the model produced them, before any host normalization.
pub(crate) const TOOL_RAW_INPUT: &str = "/transcript_event/metadata/raw_input";
/// Lifecycle status of a tool call: `pending`, `in_progress`, `completed`,
/// `failed`, or `rejected`.
pub(crate) const TOOL_STATUS: &str = "/transcript_event/metadata/status";
/// Wall-clock duration of a completed tool call.
pub(crate) const TOOL_DURATION_MS: &str = "/transcript_event/metadata/duration_ms";
/// Set on a `tool_result` whose payload is an error rather than a product.
pub(crate) const TOOL_IS_ERROR: &str = "/transcript_event/metadata/is_error";

/// Which phase of the agent loop a `loop_checkpoint` marks, e.g.
/// `"iteration_start"`.
pub(crate) const CHECKPOINT_KIND: &str = "/transcript_event/metadata/kind";
/// Zero-or-one-based iteration counter carried by a `loop_checkpoint`.
pub(crate) const ITERATION: &str = "/transcript_event/metadata/iteration";

/// The common transcript metadata object carried by audit events.
pub(crate) const TRANSCRIPT_METADATA: &str = "/transcript_event/metadata";
/// Canonical plan document and mutation carried by a `plan_document` event.
pub(crate) const PLAN_DOCUMENT: &str = "/transcript_event/metadata/plan_document";
pub(crate) const PLAN_DOCUMENT_EVENT: &str = "/transcript_event/metadata/plan_document_event";

/// Human-visible text, preferring the transcript envelope and falling back to
/// the raw provider message the envelope was built from.
pub(crate) const TEXT: [&str; 2] = ["/transcript_event/text", "/raw_message/content"];
/// Speaker of an event, as the transcript or the raw message records it.
pub(crate) const ROLE: [&str; 2] = ["/transcript_event/role", "/raw_message/role"];
/// Visibility of a transcript event. Older raw provider messages do not carry
/// this field; readers decide whether that legacy absence is safe for their
/// projection. An explicit non-public value must never be treated as public.
pub(crate) const VISIBILITY: &str = "/transcript_event/visibility";
/// Stable transcript-envelope id when the producer supplied one.
pub(crate) const TRANSCRIPT_EVENT_ID: &str = "/transcript_event/id";
/// Tool name across the transcript envelope and both raw-message placements.
pub(crate) const TOOL_NAME_ANY: [&str; 3] = [
    TOOL_NAME,
    "/raw_message/name",
    "/raw_message/tool_calls/0/name",
];
/// Tool arguments across the transcript envelope and both raw-message
/// placements. Distinct from [`TOOL_RAW_INPUT`], which is only the envelope's
/// pre-normalization copy.
pub(crate) const TOOL_INPUT_ANY: [&str; 3] = [
    "/transcript_event/metadata/input",
    "/raw_message/input",
    "/raw_message/tool_calls/0/arguments",
];
/// Tool output, preferring the structured metadata copy over the rendered text
/// a human would read.
pub(crate) const TOOL_OUTPUT_ANY: [&str; 3] = [
    "/transcript_event/metadata/output",
    "/raw_message/content",
    "/transcript_event/text",
];
/// Error marker across the audit envelope and the provider-neutral raw result.
pub(crate) const TOOL_IS_ERROR_ANY: [&str; 2] = [TOOL_IS_ERROR, "/raw_message/is_error"];
/// Provider-neutral tool-result facts are storage-only and stripped before
/// provider egress.
pub(crate) const TOOL_RESULT_FACT_CALL_ID: &str = "/raw_message/_harn/tool_call_id";
/// The only verification fact this projection accepts: a typed deterministic
/// postcondition emitted by the tool producer.
pub(crate) const TOOL_VERIFICATION: &str = "/raw_message/_harn/data/verification";

/// First pointer that resolves to a non-empty trimmed string.
///
/// Pointers are tried in order, so callers express preference by ordering
/// rather than by chaining `or_else` at every call site.
pub(crate) fn semantic_string(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// First pointer that resolves at all, cloned.
pub(crate) fn semantic_value(payload: &Value, pointers: &[&str]) -> Option<Value> {
    pointers
        .iter()
        .find_map(|pointer| payload.pointer(pointer))
        .cloned()
}

/// Read one pointer as a string.
pub(crate) fn string_at(payload: &Value, pointer: &str) -> Option<String> {
    semantic_string(payload, &[pointer])
}

/// Read one pointer as a signed integer.
///
/// A JSON number that arrived as a float — which providers do emit for token
/// counts — is truncated rather than rejected, because a token count is
/// integral in meaning regardless of how it was serialized.
pub(crate) fn i64_at(payload: &Value, pointer: &str) -> Option<i64> {
    let value = payload.pointer(pointer)?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number as i64))
}

/// Read one pointer as a float.
pub(crate) fn f64_at(payload: &Value, pointer: &str) -> Option<f64> {
    payload.pointer(pointer).and_then(Value::as_f64)
}

/// Read one pointer as a boolean, defaulting to `false` when absent.
pub(crate) fn bool_at(payload: &Value, pointer: &str) -> bool {
    payload
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether any accepted pointer resolves to JSON `true`.
pub(crate) fn bool_at_any(payload: &Value, pointers: &[&str]) -> bool {
    pointers.iter().any(|pointer| bool_at(payload, pointer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payload shaped exactly as the session store persists an `llm_call`,
    /// copied from a real headless run rather than invented here, so the
    /// pointer constants are checked against the emitter's actual shape.
    fn llm_call_payload() -> Value {
        serde_json::json!({
            "transcript_event": {
                "blocks": [{"text": "LLM call completed", "type": "text"}],
                "id": "019fc7e6-71f4-7583-8664-55fdb52f962e",
                "kind": "llm_call",
                "metadata": {
                    "cache_read_tokens": 0,
                    "cache_write_tokens": 10948,
                    "canonical_stop_reason": "end_turn",
                    "cost_usd": 0.002872,
                    "input_tokens": 10951,
                    "model": "gpt-5.6-luna",
                    "output_tokens": 112,
                    "provider": "openai",
                    "provider_stop_reason": "completed"
                },
                "role": "assistant",
                "text": "LLM call completed"
            }
        })
    }

    #[test]
    fn typed_accessors_read_a_real_llm_call_payload() {
        let payload = llm_call_payload();
        assert_eq!(string_at(&payload, MODEL).as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(string_at(&payload, PROVIDER).as_deref(), Some("openai"));
        assert_eq!(i64_at(&payload, INPUT_TOKENS), Some(10951));
        assert_eq!(i64_at(&payload, OUTPUT_TOKENS), Some(112));
        assert_eq!(i64_at(&payload, CACHE_WRITE_TOKENS), Some(10948));
        assert_eq!(f64_at(&payload, COST_USD), Some(0.002872));
        assert_eq!(
            semantic_string(&payload, &TEXT).as_deref(),
            Some("LLM call completed")
        );
        assert_eq!(
            semantic_string(&payload, &ROLE).as_deref(),
            Some("assistant")
        );
    }

    #[test]
    fn absent_and_blank_facts_read_as_missing_rather_than_empty() {
        let payload = serde_json::json!({
            "transcript_event": {"text": "   ", "metadata": {"model": ""}}
        });
        // A whitespace-only or empty string is the emitter declining to say,
        // not a value. Callers that fall back to another source depend on this
        // being `None` rather than `Some("")`.
        assert_eq!(string_at(&payload, MODEL), None);
        assert_eq!(semantic_string(&payload, &TEXT), None);
        assert_eq!(i64_at(&payload, INPUT_TOKENS), None);
        assert_eq!(f64_at(&payload, COST_USD), None);
        assert!(!bool_at(&payload, TOOL_IS_ERROR));
    }

    #[test]
    fn token_counts_serialized_as_floats_still_read_as_integers() {
        // Some providers report usage as JSON floats. Rejecting those would
        // silently drop the whole call's usage from an aggregate.
        let payload = serde_json::json!({
            "transcript_event": {"metadata": {"input_tokens": 1024.0}}
        });
        assert_eq!(i64_at(&payload, INPUT_TOKENS), Some(1024));
    }

    #[test]
    fn text_prefers_the_transcript_envelope_over_the_raw_message() {
        let payload = serde_json::json!({
            "transcript_event": {"text": "envelope"},
            "raw_message": {"content": "raw"}
        });
        assert_eq!(
            semantic_string(&payload, &TEXT).as_deref(),
            Some("envelope")
        );

        // With no envelope text the raw message is the only source left, and
        // dropping to it must not require the caller to know the order.
        let raw_only = serde_json::json!({"raw_message": {"content": "raw"}});
        assert_eq!(semantic_string(&raw_only, &TEXT).as_deref(), Some("raw"));
    }
}
