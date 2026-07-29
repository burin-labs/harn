use super::SubAgentRunSpec;
use crate::value::VmError;

#[derive(Clone)]
pub(super) struct SubagentStopDetails {
    pub(super) status: crate::agent_events::SubagentTerminalStatus,
    pub(super) terminal_class: String,
    pub(super) reason: String,
    pub(super) cancellation: Option<serde_json::Value>,
    pub(super) timeout: Option<serde_json::Value>,
}

impl SubagentStopDetails {
    pub(super) fn success() -> Self {
        Self {
            status: crate::agent_events::SubagentTerminalStatus::Success,
            terminal_class: "completed".to_string(),
            reason: "sub-agent returned successfully".to_string(),
            cancellation: None,
            timeout: None,
        }
    }

    pub(super) fn failure(class: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: crate::agent_events::SubagentTerminalStatus::Failure,
            terminal_class: class.into(),
            reason: reason.into(),
            cancellation: None,
            timeout: None,
        }
    }
}

pub(super) fn emit_subagent_stop_once(spec: &SubAgentRunSpec, details: SubagentStopDetails) {
    use std::sync::atomic::Ordering;

    let Some(parent_run_id) = spec.parent_session_id.as_ref() else {
        return;
    };
    if spec
        .stop_emitted
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::SubagentStop {
        session_id: parent_run_id.clone(),
        parent_run_id: parent_run_id.clone(),
        child_run_id: spec.session_id.clone(),
        terminal_status: details.status,
        terminal_class: details.terminal_class,
        reason: details.reason,
        result_ref: Some(format!("agent-session:{}", spec.session_id)),
        receipt_ref: Some(format!(
            "agent-session:{}#sub_agent_result",
            spec.session_id
        )),
        cancellation: details.cancellation,
        timeout: details.timeout,
        completed_at_ms: crate::clock_mock::now_ms(),
    });
}

pub(super) fn stop_details_for_error(error: &VmError) -> SubagentStopDetails {
    let category = crate::value::error_to_category(error);
    let reason = error.to_string();
    match category {
        crate::value::ErrorCategory::Cancelled => SubagentStopDetails {
            status: crate::agent_events::SubagentTerminalStatus::Cancellation,
            terminal_class: "cancelled".to_string(),
            reason: reason.clone(),
            cancellation: Some(serde_json::json!({"source": "sub_agent_run", "reason": reason})),
            timeout: None,
        },
        crate::value::ErrorCategory::Timeout => SubagentStopDetails {
            status: crate::agent_events::SubagentTerminalStatus::Timeout,
            terminal_class: "timeout".to_string(),
            reason: reason.clone(),
            cancellation: None,
            timeout: Some(serde_json::json!({"source": "sub_agent_run", "reason": reason})),
        },
        other => SubagentStopDetails::failure(other.as_str(), reason),
    }
}

pub(super) fn stop_details_for_result(result: &serde_json::Value) -> SubagentStopDetails {
    let status = result
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("done");
    let reason = result
        .get("stop_reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(status);
    let error_category = result
        .pointer("/error/category")
        .and_then(serde_json::Value::as_str);
    if error_category == Some(crate::value::ErrorCategory::Timeout.as_str()) {
        return SubagentStopDetails {
            status: crate::agent_events::SubagentTerminalStatus::Timeout,
            terminal_class: "timeout".to_string(),
            reason: reason.to_string(),
            cancellation: None,
            timeout: Some(serde_json::json!({
                "source": "agent_loop",
                "reason": reason,
            })),
        };
    }

    let terminal_class = result
        .get("terminal_class")
        .and_then(serde_json::Value::as_str);
    let terminal_kind = crate::agent_events::classify_agent_terminal(
        status,
        reason,
        result.get("error").is_some_and(|value| !value.is_null()),
        terminal_class,
    );
    match terminal_kind {
        crate::agent_events::AgentTerminalKind::Natural => SubagentStopDetails::success(),
        crate::agent_events::AgentTerminalKind::UserCancelled => SubagentStopDetails {
            status: crate::agent_events::SubagentTerminalStatus::Cancellation,
            terminal_class: terminal_kind.as_str().to_string(),
            reason: reason.to_string(),
            cancellation: Some(serde_json::json!({
                "source": "agent_loop",
                "reason": reason,
            })),
            timeout: None,
        },
        _ => SubagentStopDetails::failure(terminal_kind.as_str(), reason),
    }
}
