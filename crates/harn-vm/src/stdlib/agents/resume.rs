use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use harn_parser::diagnostic_codes::Code;

use super::agents_workers::{
    emit_worker_event, ensure_worker_config_session_ids, load_worker_state_snapshot,
    persist_worker_state_snapshot, with_worker_state, worker_event_snapshot, worker_summary,
    WorkerConfig, WorkerState, WorkerSuspension, WORKER_REGISTRY,
};
use super::{diagnostic_error, is_nil, replay::*};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

/// Resume a suspended or persisted worker.
///
/// Accepts either:
/// - A worker handle dict or task handle currently in the local registry —
///   if it is in `suspended` state, clears the suspension and warm-resumes
///   it (transitioning back to `running` and optionally wiring resume
///   input into the next task slot).
/// - A snapshot path or worker id whose snapshot lives on disk — rehydrates
///   the snapshot into the registry. If the snapshot recorded a suspended
///   state it is preserved (the caller can then re-issue resume to warm
///   it up); otherwise it lands in the snapshot's terminal/awaiting state.
#[derive(Clone)]
pub(super) struct WorkerResumeOptions {
    pub(super) resume_input: Option<VmValue>,
    pub(super) continue_transcript: bool,
    pub(super) initiator: Option<String>,
    pub(super) trigger_event: Option<serde_json::Value>,
}

impl Default for WorkerResumeOptions {
    fn default() -> Self {
        Self {
            resume_input: None,
            continue_transcript: true,
            initiator: None,
            trigger_event: None,
        }
    }
}

pub(super) fn resume_rejected_error(worker: &WorkerState) -> VmError {
    if worker.status == "cancelled" {
        diagnostic_error(
            Code::ResumeWorkerClosed,
            format!(
                "resume_agent: worker {} was closed and cannot be resumed",
                worker.id
            ),
        )
    } else {
        diagnostic_error(
            Code::ResumeWorkerNotSuspended,
            format!(
                "resume_agent: worker {} is not suspended (status={})",
                worker.id, worker.status
            ),
        )
    }
}

pub(super) fn parse_resume_options(args: &[VmValue]) -> WorkerResumeOptions {
    let mut options = WorkerResumeOptions::default();
    let Some(raw) = args.get(1) else {
        return options;
    };
    if let VmValue::Dict(dict) = raw {
        let has_options = dict.contains_key("input")
            || dict.contains_key("resume_input")
            || dict.contains_key("continue_transcript")
            || dict.contains_key("initiator")
            || dict.contains_key("resume_initiator")
            || dict.contains_key("trigger_event");
        if has_options {
            options.resume_input = dict
                .get("input")
                .or_else(|| dict.get("resume_input"))
                .filter(|value| !is_nil(value))
                .cloned();
            if let Some(flag) = dict
                .get("continue_transcript")
                .filter(|value| !is_nil(value))
            {
                options.continue_transcript = flag.is_truthy();
            }
            options.initiator = dict
                .get("initiator")
                .or_else(|| dict.get("resume_initiator"))
                .filter(|value| !is_nil(value))
                .map(VmValue::display)
                .filter(|value| !value.trim().is_empty());
            options.trigger_event = dict
                .get("trigger_event")
                .filter(|value| !is_nil(value))
                .map(crate::llm::vm_value_to_json);
        } else {
            options.resume_input = Some(raw.clone());
        }
    } else if !is_nil(raw) {
        options.resume_input = Some(raw.clone());
    }
    if let Some(flag) = args.get(2).filter(|value| !is_nil(value)) {
        options.continue_transcript = flag.is_truthy();
    }
    options
}

pub(super) fn summary_message(summary: &str) -> VmValue {
    VmValue::dict(BTreeMap::from([
        (
            "role".to_string(),
            VmValue::String(arcstr::ArcStr::from("user".to_string())),
        ),
        (
            "content".to_string(),
            VmValue::String(arcstr::ArcStr::from(summary.to_string())),
        ),
    ]))
}

pub(super) fn summary_only_resume_transcript(
    transcript: Option<VmValue>,
    digest_override: Option<String>,
) -> Option<VmValue> {
    let transcript = transcript?;
    let dict = transcript.as_dict()?;
    let summary = digest_override.or_else(|| crate::llm::helpers::transcript_summary_text(dict));
    let summary = summary.as_deref()?.trim();
    if summary.is_empty() {
        return None;
    }
    Some(crate::llm::helpers::new_transcript_with(
        crate::llm::helpers::transcript_id(dict),
        vec![summary_message(summary)],
        Some(summary.to_string()),
        dict.get("metadata").cloned(),
    ))
}

pub(super) async fn resume_digest_from_transcript(
    ctx: &AsyncBuiltinCtx,
    transcript: Option<&VmValue>,
) -> Result<Option<String>, VmError> {
    let Some(transcript) = transcript else {
        return Ok(None);
    };
    let Some(dict) = transcript.as_dict() else {
        return Ok(None);
    };
    if let Some(summary) = crate::llm::helpers::transcript_summary_text(dict) {
        let summary = summary.trim();
        if !summary.is_empty() {
            return Ok(Some(summary.to_string()));
        }
    }
    let messages = crate::llm::helpers::transcript_message_list(dict)?;
    if messages.is_empty() {
        return Ok(None);
    }
    let mut json_messages = messages
        .iter()
        .map(crate::llm::helpers::vm_value_to_json)
        .collect::<Vec<_>>();
    let mut config = crate::orchestration::AutoCompactConfig {
        token_threshold: 0,
        keep_last: 0,
        compact_strategy: crate::orchestration::CompactStrategy::Truncate,
        policy_strategy: crate::orchestration::compact_strategy_name(
            &crate::orchestration::CompactStrategy::Truncate,
        )
        .to_string(),
        ..Default::default()
    };
    // `ResumeDigest` mode skips hook dispatch and AgentEvent emission so
    // cold-resume digest extraction stays observably equivalent to the
    // pre-lifecycle direct `auto_compact_messages` call site.
    let lifecycle = crate::orchestration::CompactLifecycle::new(
        crate::orchestration::CompactMode::ResumeDigest,
    );
    let outcome = crate::orchestration::run_compaction_lifecycle_with_ctx(
        Some(ctx),
        &mut json_messages,
        &mut config,
        None,
        lifecycle,
    )
    .await?;
    Ok(outcome
        .map(|outcome| outcome.summary.trim().to_string())
        .filter(|summary| !summary.is_empty()))
}

pub(super) fn apply_resume_transcript_policy(
    worker: &mut WorkerState,
    continue_transcript: bool,
    digest: Option<String>,
) {
    if continue_transcript {
        return;
    }
    worker.transcript = summary_only_resume_transcript(worker.transcript.take(), digest);
}

/// Wire the optional resume input into a worker's task slot. Absent input
/// is a no-op; explicitly empty input is rejected so callers do not
/// accidentally erase the next task prompt.
pub(super) fn apply_resume_input(
    worker: &mut WorkerState,
    resume_input: Option<&VmValue>,
) -> Result<(), VmError> {
    let Some(input) = resume_input else {
        return Ok(());
    };
    let text = input.display();
    if text.trim().is_empty() {
        return Err(diagnostic_error(
            Code::ResumeInputInvalid,
            "resume_agent: resume input must be nil or non-empty text",
        ));
    }
    worker.task = text.clone();
    worker.history.push(text);
    Ok(())
}

pub(super) fn apply_resume_task_to_config(config: &mut WorkerConfig, task: &str) {
    if let WorkerConfig::SubAgent { spec } = config {
        spec.task = task.to_string();
    }
}

pub(super) fn resume_input_rendered(input: Option<&VmValue>) -> Option<String> {
    let input = input?;
    let text = input.display();
    (!text.trim().is_empty()).then_some(text)
}

pub(super) fn inferred_resume_initiator(options: &WorkerResumeOptions) -> String {
    if let Some(initiator) = options
        .initiator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return initiator.to_string();
    }
    if crate::agent_sessions::current_session_id().is_some() {
        "parent".to_string()
    } else {
        "operator".to_string()
    }
}

pub(super) fn latest_payload_i64(payload: Option<&serde_json::Value>, key: &str) -> Option<i64> {
    payload.and_then(|payload| {
        payload.get(key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
    })
}

pub(super) fn suspended_at_turn(
    worker: &WorkerState,
    suspension: Option<&WorkerSuspension>,
) -> Option<i64> {
    suspension
        .and_then(|suspension| suspension.suspended_at_turn)
        .or_else(|| latest_payload_i64(worker.latest_payload.as_ref(), "iterations_completed"))
        .or_else(|| latest_payload_i64(worker.latest_payload.as_ref(), "iterations"))
}

pub(super) fn install_resume_continuity_payload(
    worker: &mut WorkerState,
    suspension: Option<&WorkerSuspension>,
    options: &WorkerResumeOptions,
    digest: Option<&str>,
) {
    let session_id = match &worker.config {
        WorkerConfig::SubAgent { spec } => spec.session_id.clone(),
        _ => return,
    };
    let suspended_turn = suspended_at_turn(worker, suspension);
    let trigger_id = worker
        .suspension
        .as_ref()
        .or(suspension)
        .and_then(|suspension| suspension.auto_resume_trigger.as_ref())
        .map(|trigger| trigger.id.clone());
    let worker_id = worker.id.clone();
    let worker_name = worker.name.clone();
    let worker_mode = worker.mode.clone();
    let reason = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.reason.clone())
        .unwrap_or_default();
    let suspension_initiator = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.initiator);
    let suspended_at = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.suspended_at.clone());
    let snapshot_ref = worker
        .suspension
        .as_ref()
        .or(suspension)
        .map(|suspension| suspension.snapshot_ref.clone());
    let conditions = worker
        .suspension
        .as_ref()
        .or(suspension)
        .and_then(|suspension| suspension.conditions.clone());
    let initiator = inferred_resume_initiator(options);
    let input_rendered = resume_input_rendered(options.resume_input.as_ref());
    let input_json = options
        .resume_input
        .as_ref()
        .map(crate::llm::vm_value_to_json);
    let trigger_event = options.trigger_event.clone();
    let trigger = trigger_event.as_ref().map(|event| {
        serde_json::json!({
            "id": trigger_id,
            "event_id": event.get("id").and_then(|value| value.as_str()),
            "kind": event.get("kind").and_then(|value| value.as_str()),
            "provider": event.get("provider").and_then(|value| value.as_str()),
            "dedupe_key": event.get("dedupe_key").and_then(|value| value.as_str()),
            "timeout_action": event
                .get("provider_payload")
                .and_then(|payload| payload.get("raw"))
                .and_then(|raw| raw.get("on_timeout"))
                .and_then(|value| value.as_str()),
        })
    });
    let resume_initiator = if initiator == "timeout" {
        "timeout".to_string()
    } else if trigger.is_some() && initiator != "parent" && initiator != "operator" {
        "triggered".to_string()
    } else {
        initiator
    };
    let payload = serde_json::json!({
        "session_id": session_id,
        "session": {"id": session_id},
        "worker": {
            "id": worker_id,
            "name": worker_name,
            "mode": worker_mode,
        },
        "suspension": {
            "reason": reason,
            "initiator": suspension_initiator,
            "suspended_at": suspended_at,
            "snapshot_ref": snapshot_ref,
            "suspended_at_turn": suspended_turn,
            "conditions": conditions,
        },
        "reason": reason,
        "suspended_at_turn": suspended_turn.unwrap_or(0),
        "resume": {
            "initiator": resume_initiator,
            "input": input_json,
            "input_rendered": input_rendered,
            "input_present": options.resume_input.is_some(),
            "continue_transcript": options.continue_transcript,
            "digest": digest,
            "trigger": trigger,
        },
        "initiator": resume_initiator,
        "input": input_json,
        "input_rendered": input_rendered,
        "input_present": options.resume_input.is_some(),
        "continue_transcript": options.continue_transcript,
        "digest": digest,
        "turn": 0,
        "iteration": 0,
    });
    if let WorkerConfig::SubAgent { spec } = &mut worker.config {
        spec.options.insert(
            crate::value::intern_key("_resume_continuity"),
            crate::stdlib::json_to_vm_value(&payload),
        );
    }
}

pub(super) async fn unregister_suspension_auto_resume(
    suspension: Option<WorkerSuspension>,
) -> Result<(), VmError> {
    let Some(handle) = suspension.and_then(|suspension| suspension.auto_resume_trigger) else {
        return Ok(());
    };
    crate::stdlib::triggers_stdlib::unregister_auto_resume_trigger(&handle).await
}

pub(super) async fn join_checkpointed_worker_handle(
    state: Arc<parking_lot::Mutex<WorkerState>>,
) -> Result<(), VmError> {
    let handle = {
        let mut worker = state.lock();
        let Some(handle) = worker.handle.as_ref() else {
            return Ok(());
        };
        if !handle.is_finished() {
            return Err(VmError::Runtime(format!(
                "resume_agent: worker {} has not reached its suspend checkpoint yet",
                worker.id
            )));
        }
        worker.handle.take()
    };
    if let Some(handle) = handle {
        let _ = handle
            .await
            .map_err(|error| VmError::Runtime(format!("resume_agent join error: {error}")))??;
    }
    Ok(())
}

pub(super) async fn warm_resume_worker(
    ctx: &AsyncBuiltinCtx,
    state: Arc<parking_lot::Mutex<WorkerState>>,
    mut options: WorkerResumeOptions,
) -> Result<VmValue, VmError> {
    join_checkpointed_worker_handle(state.clone()).await?;
    // PreResume lifecycle gate. Block keeps the worker suspended; Modify can
    // amend the resume input before the transcript policy runs.
    let pre_worker_id = state.lock().id.clone();
    let pre_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PreResume.as_str(),
        "worker": { "id": &pre_worker_id },
        "resume_input": options
            .resume_input
            .as_ref()
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(serde_json::Value::Null),
        "continue_transcript": options.continue_transcript,
    });
    match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PreResume,
        &pre_payload,
    )
    .await?
    {
        crate::orchestration::HookControl::Allow => {}
        crate::orchestration::HookControl::Block { reason } => {
            // Blocked resume: do not transition out of suspended.
            let mut summary = worker_summary(&state.lock())?;
            if let VmValue::Dict(map) = &mut summary {
                let mut entries = (**map).clone();
                entries.put_str("pre_resume_denied", reason);
                *map = std::sync::Arc::new(entries);
            }
            return Ok(summary);
        }
        crate::orchestration::HookControl::Modify { payload } => {
            if let Some(new_input) = payload.get("resume_input") {
                if !new_input.is_null() {
                    options.resume_input = Some(crate::stdlib::json_to_vm_value(new_input));
                }
            }
        }
        crate::orchestration::HookControl::Decision { .. } => {}
    }
    let resume_digest = if options.continue_transcript {
        None
    } else {
        let transcript = state.lock().transcript.clone();
        resume_digest_from_transcript(ctx, transcript.as_ref()).await?
    };
    let (worker_id, span_links) = {
        let worker = state.lock();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "resume_agent: worker {} changed before resume could complete (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        let mut span_links = Vec::new();
        if let Some(suspension) = &worker.suspension {
            if let Some(link) = suspension.prior_span_link.clone() {
                span_links.push(link.with_attributes(BTreeMap::from([(
                    "harn.link.kind".to_string(),
                    "suspension".to_string(),
                )])));
            }
            if let Some(link) = suspension.pipeline_span_link.clone() {
                span_links.push(link.with_attributes(BTreeMap::from([(
                    "harn.link.kind".to_string(),
                    "pipeline".to_string(),
                )])));
            }
        }
        (worker.id.clone(), span_links)
    };
    let link_count = span_links.len();
    let mut resume_span = LifecycleSpanGuard::start_detached(
        crate::tracing::SpanKind::Resume,
        format!("resume {worker_id}"),
        span_links,
    );
    // Annotate the resume span with its shape (initiator, transcript
    // continuity, fresh input) and prior-span link count. The resume input can
    // contain transcript text or user data, so the span records only whether
    // fresh input exists.
    annotate_resume_span(
        &resume_span,
        &worker_id,
        &inferred_resume_initiator(&options),
        options.continue_transcript,
        options.resume_input.is_some(),
        link_count,
    );
    let (snapshot, suspension) = {
        let mut worker = state.lock();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "resume_agent: worker {} changed before resume could complete (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        worker.suspend_signal.store(false, Ordering::SeqCst);
        let suspension = worker.suspension.take();
        worker.status = "running".to_string();
        worker.finished_at = None;
        worker.latest_error = None;
        apply_resume_transcript_policy(
            &mut worker,
            options.continue_transcript,
            resume_digest.clone(),
        );
        apply_resume_input(&mut worker, options.resume_input.as_ref())?;
        let task = worker.task.clone();
        apply_resume_task_to_config(&mut worker.config, &task);
        install_resume_continuity_payload(
            &mut worker,
            suspension.as_ref(),
            &options,
            resume_digest.as_deref(),
        );
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        (worker_event_snapshot(&worker), suspension)
    };
    unregister_suspension_auto_resume(suspension).await?;
    emit_worker_event(
        Some(ctx),
        &snapshot,
        crate::agent_events::WorkerEvent::WorkerResumed,
    )
    .await?;
    resume_span.end();
    respawn_worker_task(state.clone(), ctx)?;
    // PostResume hooks are advisory; the resumed turn is about to start.
    let post_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PostResume.as_str(),
        "worker": { "id": &worker_id },
    });
    crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PostResume,
        &post_payload,
    )
    .await?;
    let summary = worker_summary(&state.lock())?;
    Ok(summary)
}

pub(super) fn auto_resume_event_payload(
    event: &crate::triggers::TriggerEvent,
) -> serde_json::Value {
    let payload = serde_json::to_value(&event.provider_payload).unwrap_or(serde_json::Value::Null);
    payload.get("raw").cloned().unwrap_or(payload)
}

pub(super) fn auto_resume_timeout_action(payload: &serde_json::Value) -> Option<String> {
    if payload.get("type").and_then(|value| value.as_str()) != Some("auto_resume.timeout") {
        return None;
    }
    payload
        .get("on_timeout")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(super) async fn fail_suspended_worker_from_auto_resume_timeout(
    ctx: &AsyncBuiltinCtx,
    worker_id: &str,
) -> Result<VmValue, VmError> {
    let state = with_worker_state(worker_id, Ok)?;
    let (snapshot, summary, suspension) = {
        let mut worker = state.lock();
        if worker.status != "suspended" {
            return Err(diagnostic_error(
                Code::ConcurrentResumeConflict,
                format!(
                    "auto-resume timeout: worker {} is no longer suspended (status={})",
                    worker.id, worker.status
                ),
            ));
        }
        worker.suspend_signal.store(false, Ordering::SeqCst);
        let suspension = worker.suspension.take();
        worker.status = "failed".to_string();
        worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
        worker.latest_error = Some("auto-resume timeout expired".to_string());
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
        (
            worker_event_snapshot(&worker),
            worker_summary(&worker)?,
            suspension,
        )
    };
    unregister_suspension_auto_resume(suspension).await?;
    emit_worker_event(
        Some(ctx),
        &snapshot,
        crate::agent_events::WorkerEvent::WorkerFailed,
    )
    .await?;
    Ok(summary)
}

pub(super) fn cold_load_worker(
    snapshot_target: &str,
) -> Result<Arc<parking_lot::Mutex<WorkerState>>, VmError> {
    let state = Arc::new(parking_lot::Mutex::new(
        load_worker_state_snapshot(snapshot_target).map_err(|error| {
            diagnostic_error(
                Code::ResumeSnapshotInvalid,
                format!("resume_agent: failed to load snapshot `{snapshot_target}`: {error}"),
            )
        })?,
    ));
    let worker_id = state.lock().id.clone();
    {
        let mut worker = state.lock();
        ensure_worker_config_session_ids(&mut worker.config, &worker_id);
    }
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(worker_id.clone(), state.clone());
    });
    if state.lock().carry_policy.persist_state {
        persist_worker_state_snapshot(&state.lock())?;
    }
    Ok(state)
}
