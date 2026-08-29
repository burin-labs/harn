use super::{SubAgentExecutionResult, SubAgentRunSpec};
use crate::orchestration::{annotate_nested_execution_options, NestedExecutionKind};
use crate::value::{DictMap, VmDictExt, VmError, VmValue};

fn annotate_subagent_session(options: &mut DictMap, name: &str, parent_session_id: Option<&str>) {
    annotate_nested_execution_options(options, NestedExecutionKind::SubAgentRun, name);
    if let Some(parent_session_id) = parent_session_id {
        options.put_str("parent_session_id", parent_session_id);
    }
    options.put_str("session_type", "subagent");
}

pub(super) fn sub_agent_start_event(spec: &SubAgentRunSpec) -> VmValue {
    crate::llm::helpers::transcript_event(
        "sub_agent_start",
        "system",
        "internal",
        &spec.task,
        Some(serde_json::json!({
            "name": spec.name,
            "child_session_id": spec.session_id,
            "child_run_id": spec.run_id,
            "parent_run_id": spec.parent_run_id,
            "task": spec.task,
        })),
    )
}

pub(super) fn initialize_run_identity(
    options: &mut DictMap,
    name: &str,
) -> (String, Option<String>, Option<String>) {
    let run_id = format!("agent_run_{}", uuid::Uuid::now_v7());
    let active_parent = crate::runtime_context::current_agent_run_ref();
    let parent_session_id = active_parent
        .as_ref()
        .map(|parent| parent.session_id.clone())
        .or_else(crate::llm::current_agent_session_id);
    let parent_run_id = active_parent.map(|parent| parent.run_id).or_else(|| {
        crate::orchestration::current_mutation_session().and_then(|session| session.run_id)
    });
    options.put_str("run_id", run_id.clone());
    annotate_subagent_session(options, name, parent_session_id.as_deref());
    if let Some(parent_run_id) = parent_run_id.as_deref() {
        options.put_str("parent_run_id", parent_run_id);
    }
    (run_id, parent_session_id, parent_run_id)
}

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

    let (Some(parent_session_id), Some(parent_run_id)) =
        (spec.parent_session_id.as_ref(), spec.parent_run_id.as_ref())
    else {
        return;
    };
    if spec
        .stop_emitted
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let Some(lineage) = delegated_run_lineage(spec) else {
        return;
    };
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::SubagentStop {
        session_id: parent_session_id.clone(),
        lineage: Some(lineage),
        parent_run_id: parent_run_id.clone(),
        child_run_id: spec.run_id.clone(),
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

pub(super) fn delegated_run_lineage(
    spec: &SubAgentRunSpec,
) -> Option<crate::agent_events::DelegatedRunLineage> {
    let parent_session_id = spec.parent_session_id.as_ref()?;
    let parent_run_id = spec.parent_run_id.as_ref()?;
    if spec.session_id.is_empty() || spec.run_id.is_empty() {
        return None;
    }
    Some(crate::agent_events::DelegatedRunLineage {
        parent: crate::agent_events::AgentRunRef {
            session_id: parent_session_id.clone(),
            run_id: parent_run_id.clone(),
        },
        child: crate::agent_events::AgentRunRef {
            session_id: spec.session_id.clone(),
            run_id: spec.run_id.clone(),
        },
    })
}

pub(super) fn normalize_run_identity(spec: &mut SubAgentRunSpec) {
    if spec.run_id.trim().is_empty() {
        spec.run_id = format!("agent_run_{}", uuid::Uuid::now_v7());
    }
    if spec.parent_run_id.is_none() {
        spec.parent_run_id = crate::runtime_context::current_agent_run_ref()
            .filter(|parent| spec.parent_session_id.as_deref() == Some(parent.session_id.as_str()))
            .map(|parent| parent.run_id)
            .or_else(|| {
                crate::orchestration::current_mutation_session().and_then(|session| session.run_id)
            });
    }
    spec.options.put_str("run_id", spec.run_id.clone());
    if let Some(parent_run_id) = spec.parent_run_id.as_deref() {
        spec.options.put_str("parent_run_id", parent_run_id);
    }
}

pub(super) fn finish_sub_agent(
    spec: &SubAgentRunSpec,
    mut payload: serde_json::Value,
    transcript: VmValue,
    details: SubagentStopDetails,
) -> SubAgentExecutionResult {
    if let Some(object) = payload.as_object_mut() {
        object.insert("run_id".into(), spec.run_id.clone().into());
    }
    emit_subagent_stop_once(spec, details);
    SubAgentExecutionResult {
        payload,
        transcript,
        identity: crate::agent_events::AgentRunRef {
            session_id: spec.session_id.clone(),
            run_id: spec.run_id.clone(),
        },
    }
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
