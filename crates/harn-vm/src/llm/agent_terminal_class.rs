//! Terminal-class taxonomy for a finalized agent-loop turn.
//!
//! One place classifies *why* a turn ended into a stable `&'static str`
//! `terminal_class` — `context_overflow`, `provider_misconfigured`,
//! `rate_limited`, `timeout`, `tool_policy_rejected`,
//! `host_bridge_unimplemented`, `agent_loop_protocol_failure`, or the
//! `generic_throw` fallback. A structured error envelope (typed `category` /
//! `reason` / `code` fields) is authoritative; legacy free-text matching is a
//! Harn-side fallback only. `host_agent_session_finalize` consumes the class
//! both to shape the terminal transcript event and to split an error into
//! provider- vs harness-owned in the typed terminal outcome
//! ([`super::super::agent_events::classify_agent_terminal`]).
//!
//! Split out of `agent_session_host` so the finalize host stays focused on
//! session lifecycle while this self-contained string taxonomy — and its unit
//! tests — live together.

pub(crate) fn session_status_indicates_error(final_status: &str) -> bool {
    matches!(
        final_status,
        "error" | "failed" | "provider_error" | "verify_exhausted" | "verify_capped" | "stuck"
    )
}

/// Detect a model-less agent turn: the loop finalized a *completed* turn
/// (empty status or `done`) but never actually called the provider. We
/// treat "no iterations AND no tokens recorded for this session" as the
/// signal, since any real provider round-trip increments iterations and
/// records token usage.
///
/// Only the success-completion statuses qualify. Intentional non-terminal
/// states — `suspended`, `blocked`, `paused`, `cancelled`, waitpoints — and
/// already-errored turns legitimately finalize with zero iterations and must
/// be left alone; otherwise we would turn an intentional pause into a
/// spurious failure.
pub(crate) fn agent_turn_made_no_llm_call(
    final_status: &str,
    has_terminal_error: bool,
    iterations: i64,
    input_tokens: i64,
    output_tokens: i64,
) -> bool {
    let is_success_completion = final_status.is_empty() || final_status == "done";
    !has_terminal_error
        && is_success_completion
        && iterations == 0
        && input_tokens == 0
        && output_tokens == 0
}

pub(crate) fn agent_terminal_class(
    final_status: &str,
    stop_reason: &str,
    terminal_error: Option<&serde_json::Value>,
) -> Option<&'static str> {
    if terminal_error.is_none() && !session_status_indicates_error(final_status) {
        return None;
    }
    if let Some(error) = terminal_error {
        if let Some(class) = agent_terminal_class_from_structured_error(error) {
            return Some(class);
        }
        if let Some(class) = agent_terminal_class_from_legacy_text(error) {
            return Some(class);
        }
    }
    if terminal_status_signal_matches(final_status, stop_reason, |signal| {
        matches!(
            signal,
            "context_overflow"
                | "no_llm_call"
                | "provider_misconfigured"
                | "provider_not_configured"
                | "rate_limit"
                | "rate_limited"
                | "timeout"
                | "timed_out"
                | "deadline_exceeded"
                | "tool_policy_rejected"
                | "tool_rejected"
                | "permission_denied"
                | "policy_denied"
                | "agent_loop_protocol_failure"
        )
    }) {
        return terminal_class_from_exact_signal(final_status)
            .or_else(|| terminal_class_from_exact_signal(stop_reason));
    }
    Some("generic_throw")
}

fn agent_terminal_class_from_structured_error(error: &serde_json::Value) -> Option<&'static str> {
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(signal, &["context_overflow"])
    }) {
        return Some("context_overflow");
    }
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(signal, &["no_llm_call"])
            || terminal_signal_contains_any(
                signal,
                &[
                    "provider_misconfigured",
                    "provider_not_configured",
                    "model_not_configured",
                    "missing_api_key",
                    "missing api key",
                    "unauthorized",
                    "authentication",
                    "credential",
                ],
            )
    }) {
        return Some("provider_misconfigured");
    }
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(signal, &["rate_limit", "rate limit", "rate_limited"])
            || signal == "429"
    }) {
        return Some("rate_limited");
    }
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(signal, &["timeout", "timed out", "deadline_exceeded"])
    }) {
        return Some("timeout");
    }
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(
            signal,
            &[
                "tool_policy_rejected",
                "tool_rejected",
                "permission_denied",
                "policy_denied",
                "execution_policy",
                "exceeds execution policy",
            ],
        )
    }) {
        return Some("tool_policy_rejected");
    }
    if terminal_error_signal_matches(error, |signal| {
        signal == "-32601"
            || terminal_signal_contains_any(
                signal,
                &[
                    "host_bridge_unimplemented",
                    "method_not_found",
                    "not_implemented",
                    "not implemented",
                ],
            )
    }) {
        return Some("host_bridge_unimplemented");
    }
    if terminal_error_signal_matches(error, |signal| {
        terminal_signal_contains_any(
            signal,
            &[
                "agent_loop_protocol_failure",
                "invalid_request",
                "missing_tool_name",
                "missing tool_name",
                "empty_tool_name",
                "empty tool name",
                "tool_caller",
            ],
        )
    }) || terminal_error_has_after_tool_result_format(error)
    {
        return Some("agent_loop_protocol_failure");
    }
    None
}

fn agent_terminal_class_from_legacy_text(error: &serde_json::Value) -> Option<&'static str> {
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["context_overflow"])
    }) {
        return Some("context_overflow");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["no_llm_call"])
            || (terminal_signal_contains_any(
                text,
                &[
                    "not configured",
                    "no llm",
                    "no model",
                    "missing api key",
                    "api key",
                    "credential",
                    "unauthorized",
                    "authentication",
                ],
            ) && terminal_signal_contains_any(
                text,
                &["llm", "model", "provider", "key", "credential"],
            ))
    }) {
        return Some("provider_misconfigured");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["rate_limit", "rate limit", "rate_limited", " 429 "])
    }) {
        return Some("rate_limited");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["timeout", "timed out", "deadline_exceeded"])
    }) {
        return Some("timeout");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(
            text,
            &[
                "tool_rejected",
                "permission_denied",
                "policy_denied",
                "exceeds execution policy",
                "bridged builtin",
            ],
        )
    }) {
        return Some("tool_policy_rejected");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(
            text,
            &[
                "-32601",
                "host bridge tool",
                "not implemented by burinhostresponder",
            ],
        )
    }) {
        return Some("host_bridge_unimplemented");
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(
            text,
            &[
                "invalid_request",
                "missing tool_name",
                "missing `tool_name`",
                "empty tool name",
                "empty_tool_name",
                "tool_caller",
                "agent_loop:",
                "session/prompt error",
            ],
        )
    }) {
        return Some("agent_loop_protocol_failure");
    }
    None
}

fn terminal_class_from_exact_signal(signal: &str) -> Option<&'static str> {
    match signal {
        "context_overflow" => Some("context_overflow"),
        "no_llm_call" | "provider_misconfigured" | "provider_not_configured" => {
            Some("provider_misconfigured")
        }
        "rate_limit" | "rate_limited" => Some("rate_limited"),
        "timeout" | "timed_out" | "deadline_exceeded" => Some("timeout"),
        "tool_policy_rejected" | "tool_rejected" | "permission_denied" | "policy_denied" => {
            Some("tool_policy_rejected")
        }
        "agent_loop_protocol_failure" => Some("agent_loop_protocol_failure"),
        _ => None,
    }
}

fn terminal_error_signal_matches(
    error: &serde_json::Value,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    const STRUCTURED_KEYS: &[&str] = &[
        "category",
        "error_category",
        "reason",
        "code",
        "kind",
        "phase",
        "status",
    ];
    terminal_error_key_matches(error, STRUCTURED_KEYS, predicate)
}

fn terminal_error_legacy_text_matches(
    error: &serde_json::Value,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    terminal_error_key_matches(error, &["message", "error"], predicate)
}

fn terminal_error_key_matches(
    error: &serde_json::Value,
    keys: &[&str],
    predicate: impl Fn(&str) -> bool,
) -> bool {
    keys.iter().any(|key| {
        error
            .get(*key)
            .and_then(terminal_signal_value)
            .is_some_and(|signal| predicate(&signal))
    })
}

fn terminal_status_signal_matches(
    final_status: &str,
    stop_reason: &str,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    [final_status, stop_reason]
        .into_iter()
        .filter(|signal| !signal.is_empty())
        .any(predicate)
}

fn terminal_signal_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.to_ascii_lowercase()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn terminal_signal_contains_any(signal: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| signal.contains(needle))
}

fn terminal_error_has_after_tool_result_format(error: &serde_json::Value) -> bool {
    error
        .get("tool_format")
        .is_some_and(|value| !value.is_null())
        && error
            .get("after_tool_result")
            .is_some_and(terminal_bool_signal)
}

fn terminal_bool_signal(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(value) => value.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_terminal_class_prefers_structured_error_fields() {
        let cases = [
            (json!({"category": "no_llm_call"}), "provider_misconfigured"),
            (json!({"category": "context_overflow"}), "context_overflow"),
            (
                json!({"error_category": "permission_denied"}),
                "tool_policy_rejected",
            ),
            (
                json!({"reason": "rate_limit", "provider": "anthropic"}),
                "rate_limited",
            ),
            (json!({"kind": "deadline_exceeded"}), "timeout"),
            (
                json!({"code": "-32601", "message": "host bridge tool is not implemented by BurinHostResponder"}),
                "host_bridge_unimplemented",
            ),
            (
                json!({"reason": "invalid_request", "tool_format": "native", "after_tool_result": true}),
                "agent_loop_protocol_failure",
            ),
            (
                json!({"tool_format": "native", "after_tool_result": true}),
                "agent_loop_protocol_failure",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(
                agent_terminal_class("error", "", Some(&error)),
                Some(expected),
                "error={error}"
            );
        }
    }

    #[test]
    fn agent_terminal_class_uses_legacy_text_only_as_harn_side_fallback() {
        let provider_wrapped = json!({
            "message": "session/prompt error: agent_loop: provider not configured: missing API key"
        });
        assert_eq!(
            agent_terminal_class("error", "", Some(&provider_wrapped)),
            Some("provider_misconfigured")
        );

        let protocol_wrapped = json!({
            "message": "session/prompt error [-32000]: agent_loop: tool_caller result missing `tool_name`"
        });
        assert_eq!(
            agent_terminal_class("error", "", Some(&protocol_wrapped)),
            Some("agent_loop_protocol_failure")
        );

        assert_eq!(
            agent_terminal_class("failed", "verify_exhausted", None),
            Some("generic_throw")
        );
        assert_eq!(
            agent_terminal_class(
                "error",
                "",
                Some(&json!({"tool_format": "native", "after_tool_result": false}))
            ),
            Some("generic_throw")
        );
        assert_eq!(agent_terminal_class("done", "", None), None);
    }

    #[test]
    fn agent_terminal_class_keeps_structured_fields_authoritative() {
        assert_eq!(
            agent_terminal_class(
                "error",
                "",
                Some(&json!({
                    "category": "rate_limited",
                    "message": "session/prompt error: tool_caller result missing `tool_name`",
                }))
            ),
            Some("rate_limited")
        );
        assert_eq!(
            agent_terminal_class(
                "error",
                "",
                Some(&json!({
                    "provider": "anthropic",
                    "model": "claude-sonnet",
                    "message": "plain provider envelope without class authority",
                }))
            ),
            Some("generic_throw")
        );
    }

    #[test]
    fn model_less_turn_is_flagged_as_no_llm_call() {
        // Zero iterations + zero tokens + non-error status = silent
        // short-circuit. This is the model-less turn we must fail loud on.
        assert!(agent_turn_made_no_llm_call("", false, 0, 0, 0));
        assert!(agent_turn_made_no_llm_call("done", false, 0, 0, 0));
    }

    #[test]
    fn real_turn_is_not_flagged_as_no_llm_call() {
        // Any real provider round-trip records iterations and/or tokens.
        assert!(!agent_turn_made_no_llm_call("done", false, 1, 0, 0));
        assert!(!agent_turn_made_no_llm_call("done", false, 0, 12, 0));
        assert!(!agent_turn_made_no_llm_call("done", false, 0, 0, 34));
        // Already-errored or terminal-error turns are left as-is.
        assert!(!agent_turn_made_no_llm_call("error", false, 0, 0, 0));
        assert!(!agent_turn_made_no_llm_call("failed", false, 0, 0, 0));
        assert!(!agent_turn_made_no_llm_call("", true, 0, 0, 0));
    }
}
