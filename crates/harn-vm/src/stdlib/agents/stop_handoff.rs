use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::agents_workers::{
    emit_worker_event, persist_worker_state_snapshot, with_worker_state, worker_event_snapshot,
    worker_summary, WorkerConfig, WorkerExecutionProfile, WorkerState, WORKER_REGISTRY,
};
use super::{is_nil, resume::*};
use crate::agent_events::WorkerEvent;
use crate::orchestration::ArtifactRecord;
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

#[derive(Clone, Debug)]
pub(super) struct WorkerStopOptions {
    pub(super) graceful: bool,
    pub(super) reason: String,
}

impl Default for WorkerStopOptions {
    fn default() -> Self {
        Self {
            graceful: false,
            reason: "worker stopped".to_string(),
        }
    }
}

#[derive(Clone)]
pub(super) struct StopHandoffSource {
    pub(super) worker_id: Option<String>,
    pub(super) worker_name: String,
    pub(super) worker_task: String,
    pub(super) worker_status: Option<String>,
    pub(super) worker_mode: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) parent_session_id: Option<String>,
    pub(super) transcript: Option<VmValue>,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) snapshot_path: Option<String>,
    pub(super) child_run_id: Option<String>,
    pub(super) child_run_path: Option<String>,
    pub(super) execution: Option<WorkerExecutionProfile>,
    pub(super) workspace_anchor: Option<crate::workspace_anchor::WorkspaceAnchor>,
    pub(super) token_budget: Option<i64>,
    pub(super) latest_payload: Option<serde_json::Value>,
    pub(super) latest_error: Option<String>,
}

pub(super) struct StopHandoffTree {
    pub(super) payload: serde_json::Value,
    pub(super) handoffs: Vec<serde_json::Value>,
    pub(super) descendant_worker_payloads: Vec<(String, serde_json::Value)>,
}

pub(super) fn parse_stop_options(
    args: &[VmValue],
    caller: &str,
) -> Result<WorkerStopOptions, VmError> {
    let mut options = WorkerStopOptions::default();
    let Some(raw) = args.get(1).filter(|value| !is_nil(value)) else {
        return Ok(options);
    };
    let dict = raw.as_dict().ok_or_else(|| {
        VmError::Runtime(format!(
            "{caller}: options must be a dict with optional graceful/reason fields"
        ))
    })?;
    if let Some(value) = dict.get("graceful").filter(|value| !is_nil(value)) {
        options.graceful = match value {
            VmValue::Bool(flag) => *flag,
            other => {
                return Err(VmError::Runtime(format!(
                    "{caller}: options.graceful must be bool, got {}",
                    other.type_name()
                )));
            }
        };
    }
    if let Some(value) = dict.get("reason").filter(|value| !is_nil(value)) {
        options.reason = match value {
            VmValue::String(reason) if !reason.trim().is_empty() => reason.trim().to_string(),
            VmValue::String(_) => options.reason,
            other => {
                return Err(VmError::Runtime(format!(
                    "{caller}: options.reason must be string, got {}",
                    other.type_name()
                )));
            }
        };
    }
    Ok(options)
}

pub(super) fn worker_session_id(worker: &WorkerState) -> Option<String> {
    if let WorkerConfig::SubAgent { spec } = &worker.config {
        if !spec.session_id.is_empty() {
            return Some(spec.session_id.clone());
        }
    }
    if !worker.audit.session_id.is_empty() {
        return Some(worker.audit.session_id.clone());
    }
    None
}

pub(super) fn worker_parent_session_id(worker: &WorkerState) -> Option<String> {
    match &worker.config {
        WorkerConfig::SubAgent { spec } => spec.parent_session_id.clone(),
        _ => None,
    }
    .or_else(|| worker.audit.parent_session_id.clone())
}

pub(super) fn worker_state_for_session_id(
    session_id: &str,
) -> Option<Arc<parking_lot::Mutex<WorkerState>>> {
    WORKER_REGISTRY.with(|registry| {
        for state in registry.borrow().values() {
            let worker = state.lock();
            if worker_session_id(&worker).as_deref() == Some(session_id) {
                return Some(state.clone());
            }
        }
        None
    })
}

pub(super) fn vm_value_i64(value: &VmValue) -> Option<i64> {
    match value {
        VmValue::Int(value) => Some(*value),
        VmValue::Float(value) if value.is_finite() => Some(*value as i64),
        VmValue::String(value) => value.parse::<i64>().ok(),
        _ => None,
    }
}

pub(super) fn vm_options_token_budget(options: &BTreeMap<String, VmValue>) -> Option<i64> {
    options
        .get("token_budget")
        .and_then(vm_value_i64)
        .filter(|value| *value >= 0)
}

pub(super) fn source_token_budget_from_worker_config(config: &WorkerConfig) -> Option<i64> {
    match config {
        WorkerConfig::Workflow { options, .. } => vm_options_token_budget(options),
        WorkerConfig::SubAgent { spec } => vm_options_token_budget(&spec.options),
        WorkerConfig::Stage { node, .. } => node
            .raw_model_policy
            .as_ref()
            .and_then(VmValue::as_dict)
            .and_then(vm_options_token_budget),
    }
}

pub(super) fn transcript_tokens_used_for_handoff(transcript: &VmValue) -> i64 {
    transcript
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|value| match value {
            VmValue::List(list) => Some(list),
            _ => None,
        })
        .map(|events| {
            events
                .iter()
                .filter_map(VmValue::as_dict)
                .filter_map(|dict| dict.get("metadata").and_then(VmValue::as_dict))
                .map(|metadata| {
                    metadata
                        .get("input_tokens")
                        .and_then(VmValue::as_int)
                        .unwrap_or(0)
                        .saturating_add(
                            metadata
                                .get("output_tokens")
                                .and_then(VmValue::as_int)
                                .unwrap_or(0),
                        )
                })
                .sum()
        })
        .unwrap_or(0)
}

pub(super) fn source_from_worker(worker: &WorkerState) -> StopHandoffSource {
    let session_id = worker_session_id(worker);
    let transcript = worker.transcript.clone().or_else(|| {
        session_id
            .as_deref()
            .and_then(crate::agent_sessions::transcript)
    });
    let workspace_anchor = match &worker.config {
        WorkerConfig::SubAgent { spec } => spec.workspace_anchor.clone(),
        _ => None,
    }
    .or_else(|| {
        session_id
            .as_deref()
            .and_then(crate::agent_sessions::workspace_anchor)
    });
    StopHandoffSource {
        worker_id: Some(worker.id.clone()),
        worker_name: worker.name.clone(),
        worker_task: worker.task.clone(),
        worker_status: Some(worker.status.clone()),
        worker_mode: Some(worker.mode.clone()),
        parent_session_id: worker_parent_session_id(worker).or_else(|| {
            session_id
                .as_deref()
                .and_then(crate::agent_sessions::parent_id)
        }),
        session_id,
        transcript,
        artifacts: worker.artifacts.clone(),
        snapshot_path: Some(worker.snapshot_path.clone()),
        child_run_id: worker.child_run_id.clone(),
        child_run_path: worker.child_run_path.clone(),
        execution: Some(worker.execution.clone()),
        workspace_anchor,
        token_budget: source_token_budget_from_worker_config(&worker.config),
        latest_payload: worker.latest_payload.clone(),
        latest_error: worker.latest_error.clone(),
    }
}

pub(super) fn source_from_session(session_id: &str) -> StopHandoffSource {
    StopHandoffSource {
        worker_id: None,
        worker_name: "sub-agent".to_string(),
        worker_task: format!("session {session_id}"),
        worker_status: None,
        worker_mode: None,
        session_id: Some(session_id.to_string()),
        parent_session_id: crate::agent_sessions::parent_id(session_id),
        transcript: crate::agent_sessions::transcript(session_id),
        artifacts: Vec::new(),
        snapshot_path: None,
        child_run_id: None,
        child_run_path: None,
        execution: None,
        workspace_anchor: crate::agent_sessions::workspace_anchor(session_id),
        token_budget: None,
        latest_payload: None,
        latest_error: None,
    }
}

pub(super) fn source_for_session(session_id: &str) -> StopHandoffSource {
    if let Some(state) = worker_state_for_session_id(session_id) {
        return source_from_worker(&state.lock());
    }
    source_from_session(session_id)
}

pub(super) fn source_label(source: &StopHandoffSource) -> String {
    let label = source.worker_name.trim();
    if label.is_empty() {
        "sub-agent".to_string()
    } else {
        label.to_string()
    }
}

pub(super) fn transcript_content_text(value: &VmValue) -> Option<String> {
    match value {
        VmValue::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        VmValue::List(items) => {
            let parts = items
                .iter()
                .filter_map(transcript_content_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        VmValue::Dict(dict) => dict
            .get("text")
            .or_else(|| dict.get("content"))
            .and_then(transcript_content_text),
        _ => None,
    }
}

pub(super) fn transcript_summary_for_handoff(transcript: Option<&VmValue>) -> Option<String> {
    let dict = transcript?.as_dict()?;
    if let Some(summary) = crate::llm::helpers::transcript_summary_text(dict) {
        let summary = summary.trim();
        if !summary.is_empty() {
            return Some(summary.to_string());
        }
    }
    let messages = crate::llm::helpers::transcript_message_list(dict).ok()?;
    messages.iter().rev().find_map(|message| {
        let message = message.as_dict()?;
        let role = message
            .get("role")
            .and_then(transcript_content_text)
            .unwrap_or_default();
        if matches!(role.as_str(), "assistant" | "tool" | "user") {
            message.get("content").and_then(transcript_content_text)
        } else {
            None
        }
    })
}

pub(super) fn latest_payload_summary(payload: Option<&serde_json::Value>) -> Option<String> {
    let payload = payload?;
    for key in ["summary", "reason", "outcome", "result", "error"] {
        if let Some(value) = payload.get(key).and_then(serde_json::Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(super) fn artifact_touched_entities(artifacts: &[ArtifactRecord]) -> Vec<String> {
    let mut touched = Vec::new();
    for artifact in artifacts {
        for value in [
            artifact.metadata.get("path"),
            artifact.metadata.get("uri"),
            artifact.data.as_ref().and_then(|data| data.get("path")),
            artifact.data.as_ref().and_then(|data| data.get("uri")),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(text) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !touched.iter().any(|existing| existing == text) {
                    touched.push(text.to_string());
                }
            }
        }
    }
    touched
}

pub(super) fn evidence_refs_for_source(source: &StopHandoffSource) -> Vec<serde_json::Value> {
    let mut refs = Vec::new();
    if let Some(path) = source
        .snapshot_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        refs.push(serde_json::json!({
            "kind": "worker_snapshot",
            "path": path,
        }));
    }
    if let Some(path) = source
        .child_run_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        refs.push(serde_json::json!({
            "kind": "run_record",
            "path": path,
        }));
    }
    for artifact in &source.artifacts {
        refs.push(serde_json::json!({
            "kind": artifact.kind.clone(),
            "artifact_id": artifact.id.clone(),
            "label": artifact.title.clone(),
        }));
    }
    refs
}

pub(super) fn build_stop_handoff_tree(
    source: StopHandoffSource,
    reason: &str,
    seen_sessions: &mut HashSet<String>,
) -> Result<StopHandoffTree, VmError> {
    let session_id = source.session_id.clone();
    let mut child_trees = Vec::new();
    if let Some(session_id) = session_id.as_deref() {
        if seen_sessions.insert(session_id.to_string()) {
            for child_id in crate::agent_sessions::child_ids(session_id) {
                let child_source = source_for_session(&child_id);
                child_trees.push(build_stop_handoff_tree(
                    child_source,
                    reason,
                    seen_sessions,
                )?);
            }
        }
    }

    let child_payloads = child_trees
        .iter()
        .map(|tree| tree.payload.clone())
        .collect::<Vec<_>>();
    let child_handoffs = child_trees
        .iter()
        .filter_map(|tree| tree.payload.get("handoff").cloned())
        .collect::<Vec<_>>();
    let mut flat_handoffs = Vec::new();
    let mut descendant_worker_payloads = Vec::new();
    for tree in child_trees {
        descendant_worker_payloads.extend(tree.descendant_worker_payloads);
        if let Some(worker_id) = tree
            .payload
            .get("worker_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        {
            descendant_worker_payloads.push((worker_id, tree.payload.clone()));
        }
        flat_handoffs.extend(tree.handoffs);
    }

    let summary = latest_payload_summary(source.latest_payload.as_ref())
        .or_else(|| transcript_summary_for_handoff(source.transcript.as_ref()))
        .or_else(|| source.latest_error.clone())
        .unwrap_or_else(|| {
            "Stopped before the sub-agent produced a transcript summary.".to_string()
        });
    let tokens_used = source
        .transcript
        .as_ref()
        .map(transcript_tokens_used_for_handoff)
        .unwrap_or(0);
    let tokens_remaining = source
        .token_budget
        .map(|budget| budget.saturating_sub(tokens_used).max(0));
    let workspace_anchor = source
        .workspace_anchor
        .as_ref()
        .map(crate::workspace_anchor::WorkspaceAnchor::to_json)
        .unwrap_or(serde_json::Value::Null);
    let session_id_for_metadata = session_id.clone();
    let target_id = source
        .parent_session_id
        .clone()
        .unwrap_or_else(|| "parent_agent".to_string());
    let source_persona = source_label(&source);
    let worker_id = source.worker_id.clone();
    let worker_status = source.worker_status.clone();
    let worker_mode = source.worker_mode.clone();
    let parent_session_id = source.parent_session_id.clone();
    let token_budget = source.token_budget;
    let snapshot_path = source.snapshot_path.clone();
    let child_run_id = source.child_run_id.clone();
    let child_run_path = source.child_run_path.clone();
    let execution = source.execution.clone();
    let evidence_refs = evidence_refs_for_source(&source);
    let files_or_entities_touched = artifact_touched_entities(&source.artifacts);
    let task = source.worker_task.clone();
    let handoff = serde_json::json!({
        "_type": "handoff_artifact",
        "kind": "agent_stop",
        "source_persona": source_persona,
        "target_persona_or_human": {
            "kind": "persona",
            "id": target_id,
            "label": "parent_agent",
        },
        "task": task,
        "reason": summary,
        "evidence_refs": evidence_refs,
        "files_or_entities_touched": files_or_entities_touched,
        "open_questions": [],
        "blocked_on": [],
        "requested_capabilities": ["parent_takeover"],
        "allowed_side_effects": [],
        "budget_remaining": {
            "tokens": tokens_remaining,
            "tool_calls": serde_json::Value::Null,
            "dollars": serde_json::Value::Null,
        },
        "confidence": if source.transcript.is_some() { 0.78 } else { 0.45 },
        "receipt_links": [],
        "metadata": {
            "schema": "harn.agent_stop.handoff.v1",
            "stop_reason": reason,
            "outcome": summary,
            "worker_id": worker_id,
            "worker_status": worker_status,
            "worker_mode": worker_mode,
            "session_id": session_id_for_metadata,
            "parent_session_id": parent_session_id,
            "workspace_anchor": workspace_anchor,
            "token_budget": token_budget,
            "tokens_used": tokens_used,
            "snapshot_path": snapshot_path,
            "child_run_id": child_run_id,
            "child_run_path": child_run_path,
            "execution": execution,
            "child_handoffs": child_handoffs,
        },
    });
    let handoff = crate::orchestration::normalize_handoff_artifact_json(handoff)
        .map_err(|error| VmError::Runtime(format!("agent_stop handoff error: {error}")))?;
    let handoff = serde_json::to_value(handoff)
        .map_err(|error| VmError::Runtime(format!("agent_stop handoff encode error: {error}")))?;
    flat_handoffs.insert(0, handoff.clone());

    let payload = serde_json::json!({
        "_type": "agent_stop_result",
        "schema": "harn.agent_stop.result.v1",
        "status": "stopped",
        "stop_mode": "graceful",
        "reason": reason,
        "worker_id": source.worker_id,
        "session_id": session_id,
        "summary": summary,
        "handoff": handoff,
        "children": child_payloads,
        "handoffs": flat_handoffs.clone(),
    });
    Ok(StopHandoffTree {
        payload,
        handoffs: flat_handoffs,
        descendant_worker_payloads,
    })
}

pub(super) async fn finish_worker_with_event(
    ctx: &AsyncBuiltinCtx,
    state: Arc<parking_lot::Mutex<WorkerState>>,
    status: &'static str,
    event: WorkerEvent,
    latest_error: Option<String>,
    latest_payload: Option<serde_json::Value>,
) -> Result<VmValue, VmError> {
    let (snapshot, summary, suspension) = {
        let mut worker = state.lock();
        worker.cancel_token.store(true, Ordering::SeqCst);
        worker.suspend_signal.store(false, Ordering::SeqCst);
        if let Some(handle) = worker.handle.take() {
            handle.abort();
        }
        worker.status = status.to_string();
        worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
        worker.awaiting_started_at = None;
        worker.awaiting_since = None;
        let suspension = worker.suspension.take();
        worker.latest_error = latest_error;
        if let Some(payload) = latest_payload.clone() {
            worker.latest_payload = Some(payload);
        }
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        let mut snapshot = worker_event_snapshot(&worker);
        if let Some(payload) = latest_payload {
            if let Some(metadata) = snapshot.metadata.as_object_mut() {
                metadata.insert("agent_stop".to_string(), payload.clone());
                if let Some(handoff) = payload.get("handoff") {
                    metadata.insert("handoff".to_string(), handoff.clone());
                }
            }
        }
        let summary = worker_summary(&worker)?;
        (snapshot, summary, suspension)
    };
    unregister_suspension_auto_resume(suspension).await?;
    emit_worker_event(Some(ctx), &snapshot, event).await?;
    Ok(summary)
}

pub(super) async fn stop_descendant_workers(
    ctx: &AsyncBuiltinCtx,
    descendant_payloads: Vec<(String, serde_json::Value)>,
) -> Result<(), VmError> {
    for (worker_id, payload) in descendant_payloads {
        let Some(state) = with_worker_state(&worker_id, Ok).ok() else {
            continue;
        };
        let status = state.lock().status.clone();
        if matches!(
            status.as_str(),
            "completed" | "failed" | "cancelled" | "stopped"
        ) {
            continue;
        }
        let _ = finish_worker_with_event(
            ctx,
            state,
            "stopped",
            WorkerEvent::WorkerStopped,
            None,
            Some(payload),
        )
        .await?;
    }
    Ok(())
}
