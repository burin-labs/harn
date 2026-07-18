//! Terminal-class taxonomy for a finalized agent-loop turn.
//!
//! One place classifies *why* a turn ended into a stable typed
//! [`AgentTerminalClass`] — `context_overflow`, `provider_misconfigured`,
//! `provider_unavailable`, `rate_limited`, `timeout`, `resource_busy`,
//! `tool_policy_rejected`, `host_bridge_unimplemented`,
//! `agent_loop_protocol_failure`, or the `generic_throw` fallback. A structured
//! error envelope (typed `category` /
//! `reason` / `code` fields) is authoritative; legacy free-text matching is a
//! Harn-side fallback only. `host_agent_session_finalize` consumes the class
//! both to shape the terminal transcript event and to split an error into
//! provider- vs harness-owned in the typed terminal outcome
//! ([`super::super::agent_events::classify_agent_terminal`]).
//!
//! Split out of `agent_session_host` so the finalize host stays focused on
//! session lifecycle while this self-contained taxonomy and its unit tests live
//! together.

use serde::{Deserialize, Serialize};

/// Stable fine-grained reason for an errored agent turn.
///
/// This is distinct from the coarse `AgentTerminalKind`: hosts use this class
/// for actionable recovery wording and diagnostics while the kind identifies
/// the responsible owner. Serialized values are an additive wire contract used
/// by terminal checkpoints and ACP prompt-error data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTerminalClass {
    ContextOverflow,
    ProviderMisconfigured,
    ProviderUnavailable,
    RateLimited,
    Timeout,
    ResourceBusy,
    ToolPolicyRejected,
    HostBridgeUnimplemented,
    AgentLoopProtocolFailure,
    GenericThrow,
}

impl AgentTerminalClass {
    pub const ALL: [Self; 10] = [
        Self::ContextOverflow,
        Self::ProviderMisconfigured,
        Self::ProviderUnavailable,
        Self::RateLimited,
        Self::Timeout,
        Self::ResourceBusy,
        Self::ToolPolicyRejected,
        Self::HostBridgeUnimplemented,
        Self::AgentLoopProtocolFailure,
        Self::GenericThrow,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextOverflow => "context_overflow",
            Self::ProviderMisconfigured => "provider_misconfigured",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ResourceBusy => "resource_busy",
            Self::ToolPolicyRejected => "tool_policy_rejected",
            Self::HostBridgeUnimplemented => "host_bridge_unimplemented",
            Self::AgentLoopProtocolFailure => "agent_loop_protocol_failure",
            Self::GenericThrow => "generic_throw",
        }
    }

    pub fn is_provider_error(self) -> bool {
        matches!(
            self,
            Self::ContextOverflow
                | Self::ProviderMisconfigured
                | Self::ProviderUnavailable
                | Self::RateLimited
                | Self::Timeout
        )
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "context_overflow" => Some(Self::ContextOverflow),
            "provider_misconfigured" => Some(Self::ProviderMisconfigured),
            "provider_unavailable" => Some(Self::ProviderUnavailable),
            "rate_limited" => Some(Self::RateLimited),
            "timeout" => Some(Self::Timeout),
            "resource_busy" => Some(Self::ResourceBusy),
            "tool_policy_rejected" => Some(Self::ToolPolicyRejected),
            "host_bridge_unimplemented" => Some(Self::HostBridgeUnimplemented),
            "agent_loop_protocol_failure" => Some(Self::AgentLoopProtocolFailure),
            "generic_throw" => Some(Self::GenericThrow),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentTerminalClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

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

pub fn agent_terminal_class(
    final_status: &str,
    stop_reason: &str,
    terminal_error: Option<&serde_json::Value>,
) -> Option<AgentTerminalClass> {
    if terminal_error.is_none() && !session_status_indicates_error(final_status) {
        return None;
    }
    if let Some(error) = terminal_error {
        if terminal_error_has_structured_signal(error) {
            return Some(
                agent_terminal_class_from_structured_error(error)
                    .unwrap_or(AgentTerminalClass::GenericThrow),
            );
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
                | "provider_unavailable"
                | "rate_limit"
                | "rate_limited"
                | "timeout"
                | "timed_out"
                | "deadline_exceeded"
                | "resource_busy"
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
    Some(AgentTerminalClass::GenericThrow)
}

fn agent_terminal_class_from_structured_error(
    error: &serde_json::Value,
) -> Option<AgentTerminalClass> {
    if let Some(class) = error
        .get("terminal_class")
        .and_then(serde_json::Value::as_str)
        .and_then(AgentTerminalClass::from_wire)
    {
        return Some(class);
    }
    if terminal_error_signal_matches(error, |signal| signal == "context_overflow") {
        return Some(AgentTerminalClass::ContextOverflow);
    }
    if terminal_error_signal_matches(error, |signal| signal == "no_llm_call") {
        return Some(AgentTerminalClass::ProviderMisconfigured);
    }
    if terminal_error_signal_matches(error, |signal| signal == "resource_busy") {
        return Some(AgentTerminalClass::ResourceBusy);
    }
    if terminal_error_signal_matches(error, |signal| {
        matches!(signal, "tool_rejected" | "egress_blocked")
    }) {
        return Some(AgentTerminalClass::ToolPolicyRejected);
    }
    if terminal_error_has_after_tool_result_format(error) {
        return Some(AgentTerminalClass::AgentLoopProtocolFailure);
    }
    None
}

fn agent_terminal_class_from_legacy_text(error: &serde_json::Value) -> Option<AgentTerminalClass> {
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["context_overflow"])
    }) {
        return Some(AgentTerminalClass::ContextOverflow);
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
        return Some(AgentTerminalClass::ProviderMisconfigured);
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["rate_limit", "rate limit", "rate_limited", " 429 "])
    }) {
        return Some(AgentTerminalClass::RateLimited);
    }
    if terminal_error_legacy_text_matches(error, |text| {
        terminal_signal_contains_any(text, &["timeout", "timed out", "deadline_exceeded"])
    }) {
        return Some(AgentTerminalClass::Timeout);
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
        return Some(AgentTerminalClass::ToolPolicyRejected);
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
        return Some(AgentTerminalClass::HostBridgeUnimplemented);
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
        return Some(AgentTerminalClass::AgentLoopProtocolFailure);
    }
    None
}

fn terminal_class_from_exact_signal(signal: &str) -> Option<AgentTerminalClass> {
    match signal {
        "context_overflow" => Some(AgentTerminalClass::ContextOverflow),
        "no_llm_call" | "provider_misconfigured" | "provider_not_configured" => {
            Some(AgentTerminalClass::ProviderMisconfigured)
        }
        "provider_unavailable" => Some(AgentTerminalClass::ProviderUnavailable),
        "rate_limit" | "rate_limited" => Some(AgentTerminalClass::RateLimited),
        "timeout" | "timed_out" | "deadline_exceeded" => Some(AgentTerminalClass::Timeout),
        "resource_busy" => Some(AgentTerminalClass::ResourceBusy),
        "tool_policy_rejected" | "tool_rejected" | "permission_denied" | "policy_denied" => {
            Some(AgentTerminalClass::ToolPolicyRejected)
        }
        "agent_loop_protocol_failure" => Some(AgentTerminalClass::AgentLoopProtocolFailure),
        _ => None,
    }
}

fn terminal_error_has_structured_signal(error: &serde_json::Value) -> bool {
    const STRUCTURED_KEYS: &[&str] = &[
        "category",
        "error_category",
        "reason",
        "code",
        "kind",
        "phase",
        "status",
        "terminal_class",
        "tool_format",
        "after_tool_result",
    ];
    STRUCTURED_KEYS.iter().any(|key| error.get(*key).is_some())
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
    fn terminal_class_wire_values_are_stable_and_exhaustive() {
        let pairs = [
            (AgentTerminalClass::ContextOverflow, "context_overflow"),
            (
                AgentTerminalClass::ProviderMisconfigured,
                "provider_misconfigured",
            ),
            (
                AgentTerminalClass::ProviderUnavailable,
                "provider_unavailable",
            ),
            (AgentTerminalClass::RateLimited, "rate_limited"),
            (AgentTerminalClass::Timeout, "timeout"),
            (AgentTerminalClass::ResourceBusy, "resource_busy"),
            (
                AgentTerminalClass::ToolPolicyRejected,
                "tool_policy_rejected",
            ),
            (
                AgentTerminalClass::HostBridgeUnimplemented,
                "host_bridge_unimplemented",
            ),
            (
                AgentTerminalClass::AgentLoopProtocolFailure,
                "agent_loop_protocol_failure",
            ),
            (AgentTerminalClass::GenericThrow, "generic_throw"),
        ];
        for (class, wire) in pairs {
            assert_eq!(class.as_str(), wire);
            assert_eq!(serde_json::to_value(class).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<AgentTerminalClass>(json!(wire)).unwrap(),
                class
            );
        }
        assert_eq!(pairs.len(), AgentTerminalClass::ALL.len());
    }

    #[test]
    fn agent_terminal_class_prefers_structured_error_fields() {
        let cases = [
            (
                json!({"category": "no_llm_call"}),
                AgentTerminalClass::ProviderMisconfigured,
            ),
            (
                json!({"category": "context_overflow"}),
                AgentTerminalClass::ContextOverflow,
            ),
            (
                json!({"terminal_class": "provider_unavailable"}),
                AgentTerminalClass::ProviderUnavailable,
            ),
            (
                json!({"terminal_class": "tool_policy_rejected"}),
                AgentTerminalClass::ToolPolicyRejected,
            ),
            (
                json!({"terminal_class": "rate_limited", "provider": "anthropic"}),
                AgentTerminalClass::RateLimited,
            ),
            (
                json!({"terminal_class": "timeout"}),
                AgentTerminalClass::Timeout,
            ),
            (
                json!({"category": "resource_busy"}),
                AgentTerminalClass::ResourceBusy,
            ),
            (
                json!({"terminal_class": "host_bridge_unimplemented"}),
                AgentTerminalClass::HostBridgeUnimplemented,
            ),
            (
                json!({"reason": "invalid_request", "tool_format": "native", "after_tool_result": true}),
                AgentTerminalClass::AgentLoopProtocolFailure,
            ),
            (
                json!({"tool_format": "native", "after_tool_result": true}),
                AgentTerminalClass::AgentLoopProtocolFailure,
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
            Some(AgentTerminalClass::ProviderMisconfigured)
        );

        let protocol_wrapped = json!({
            "message": "session/prompt error [-32000]: agent_loop: tool_caller result missing `tool_name`"
        });
        assert_eq!(
            agent_terminal_class("error", "", Some(&protocol_wrapped)),
            Some(AgentTerminalClass::AgentLoopProtocolFailure)
        );

        assert_eq!(
            agent_terminal_class("failed", "verify_exhausted", None),
            Some(AgentTerminalClass::GenericThrow)
        );
        assert_eq!(
            agent_terminal_class(
                "error",
                "",
                Some(&json!({"tool_format": "native", "after_tool_result": false}))
            ),
            Some(AgentTerminalClass::GenericThrow)
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
                    "terminal_class": "rate_limited",
                    "message": "session/prompt error: tool_caller result missing `tool_name`",
                }))
            ),
            Some(AgentTerminalClass::RateLimited)
        );
        for category in ["auth", "timeout", "rate_limited", "overloaded"] {
            assert_eq!(
                agent_terminal_class(
                    "error",
                    "",
                    Some(&json!({
                        "category": category,
                        "message": "provider-shaped prose without terminal provenance",
                    }))
                ),
                Some(AgentTerminalClass::GenericThrow),
                "generic VM category {category} must not claim provider provenance"
            );
        }
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
            Some(AgentTerminalClass::GenericThrow)
        );

        assert_eq!(
            agent_terminal_class(
                "error",
                "",
                Some(&json!({
                    "category": "some_future_category",
                    "message": "provider rate limit 429 in /tmp/run-429/result",
                }))
            ),
            Some(AgentTerminalClass::GenericThrow),
            "an explicit structured category must never fall through to message prose"
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
