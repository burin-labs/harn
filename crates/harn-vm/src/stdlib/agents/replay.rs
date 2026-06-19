use crate::value::VmDictExt;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, ensure_worker_config_session_ids,
    persist_worker_state_snapshot, spawn_worker_task, worker_wait_blocks, SuspendInitiator,
    WorkerConfig, WorkerState,
};
use super::SubAgentRunSpec;
use crate::orchestration::ArtifactRecord;
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

pub(super) struct WorkerReplayCarry {
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) transcript: Option<VmValue>,
    pub(super) parent_worker_id: String,
    pub(super) worker_id: String,
    pub(super) resume_workflow: bool,
    pub(super) child_run_path: Option<String>,
    pub(super) reset_sub_agent_session: bool,
}

pub(super) struct LifecycleSpanGuard {
    pub(super) span_id: u64,
    pub(super) otel_span: tracing::Span,
}

impl LifecycleSpanGuard {
    pub(super) fn start(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, true)
    }

    pub(super) fn start_detached(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
    ) -> Self {
        Self::start_with_parenting(kind, name, links, false)
    }

    pub(super) fn start_with_parenting(
        kind: crate::tracing::SpanKind,
        name: String,
        links: Vec<crate::tracing::SpanLink>,
        inherit_parent: bool,
    ) -> Self {
        let span_id = if inherit_parent {
            crate::tracing::span_start_with_links(kind, name.clone(), links.clone())
        } else {
            crate::tracing::span_start_detached_with_links(kind, name.clone(), links.clone())
        };
        let otel_span = tracing::info_span!(
            target: "harn.vm.lifecycle",
            "harn.lifecycle",
            harn.kind = kind.as_str(),
            harn.name = %name,
        );
        for link in links {
            let trace_id = crate::TraceId(link.trace_id);
            let mut attributes: HashMap<String, String> = link.attributes.into_iter().collect();
            attributes
                .entry("harn.link.kind".to_string())
                .or_insert_with(|| "causal".to_string());
            let _ = crate::observability::otel::set_span_link(
                &otel_span,
                &trace_id,
                &link.span_id,
                Some(attributes),
            );
        }
        Self { span_id, otel_span }
    }

    pub(super) fn link(&self) -> Option<crate::tracing::SpanLink> {
        crate::observability::otel::current_span_context_hex(&self.otel_span)
            .map(|(trace_id, span_id)| crate::tracing::SpanLink::new(trace_id, span_id))
            .or_else(|| crate::tracing::span_link(self.span_id))
    }

    pub(super) fn set_metadata(&self, key: &str, value: serde_json::Value) {
        crate::tracing::span_set_metadata(self.span_id, key, value);
    }

    pub(super) fn end(&mut self) {
        if self.span_id != 0 {
            crate::tracing::span_end(self.span_id);
            self.span_id = 0;
        }
    }
}

impl Drop for LifecycleSpanGuard {
    fn drop(&mut self) {
        self.end();
    }
}

/// Apply the canonical attribute bag to a suspension lifecycle span.
/// Kept in one helper so suspend_agent_builtin and
/// top_level_agent_suspend_builtin emit identically shaped spans for
/// OTel exporters and downstream queries.
///
/// Privacy: `reason` is a short human-facing label already broadcast on
/// WorkerSuspended events and PreSuspend/PostSuspend hook payloads, so
/// mirroring it onto the span attributes does not widen exposure.
/// Conditions are summarised as a boolean — the condition payload itself
/// is intentionally NOT placed on the span.
pub(super) fn annotate_suspension_span(
    span: &LifecycleSpanGuard,
    worker_id: &str,
    reason: &str,
    initiator: SuspendInitiator,
    pipeline_span_link: Option<&crate::tracing::SpanLink>,
    parent_worker_id: Option<&str>,
    has_conditions: bool,
) {
    span.set_metadata("worker_id", serde_json::json!(worker_id));
    // `handle` is the canonical OTel attribute name for suspension spans;
    // `worker_id` remains for consumers that join against worker events.
    span.set_metadata("handle", serde_json::json!(worker_id));
    span.set_metadata("reason", serde_json::json!(reason));
    span.set_metadata(
        "initiator",
        serde_json::json!(serde_json::to_value(initiator).unwrap_or(serde_json::Value::Null)),
    );
    span.set_metadata("has_conditions", serde_json::json!(has_conditions));
    if let Some(pipeline_span_link) = pipeline_span_link {
        // Worker state does not carry a distinct pipeline identifier, so the
        // active pipeline span id is the stable cross-span join key.
        span.set_metadata(
            "pipeline_span_id",
            serde_json::json!(pipeline_span_link.span_id),
        );
        span.set_metadata("pipeline_id", serde_json::json!(pipeline_span_link.span_id));
    }
    if let Some(parent_worker_id) = parent_worker_id {
        span.set_metadata("parent_worker_id", serde_json::json!(parent_worker_id));
    }
}

/// Apply the canonical attribute bag to a resume lifecycle span.
/// Booleans describe the shape of the resume (transcript
/// continuity, fresh resume input) without exposing transcript text or
/// the resume payload itself.
pub(super) fn annotate_resume_span(
    span: &LifecycleSpanGuard,
    worker_id: &str,
    initiator: &str,
    continue_transcript: bool,
    had_resume_input: bool,
    linked_suspension_count: usize,
) {
    span.set_metadata("worker_id", serde_json::json!(worker_id));
    span.set_metadata("handle", serde_json::json!(worker_id));
    span.set_metadata("initiator", serde_json::json!(initiator));
    span.set_metadata(
        "continue_transcript",
        serde_json::json!(continue_transcript),
    );
    span.set_metadata("had_resume_input", serde_json::json!(had_resume_input));
    span.set_metadata(
        "linked_suspension_count",
        serde_json::json!(linked_suspension_count),
    );
}

pub(super) fn restart_worker_run(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) -> Result<(), VmError> {
    reset_worker_for_replay(worker, next_task, clear_latest_payload)
}

pub(super) fn reset_worker_for_replay(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) -> Result<(), VmError> {
    reset_worker_runtime_state(worker, next_task, clear_latest_payload);
    let carry = worker_replay_carry(worker)?;
    worker.transcript = carry.transcript.clone();
    ensure_worker_config_session_ids(&mut worker.config, &carry.worker_id);
    apply_worker_replay_config(&mut worker.config, next_task, carry);
    Ok(())
}

pub(super) fn reset_worker_runtime_state(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) {
    worker.cancel_token = Arc::new(AtomicBool::new(false));
    worker.task = next_task.to_string();
    worker.history.push(next_task.to_string());
    worker.status = "running".to_string();
    worker.started_at = uuid::Uuid::now_v7().to_string();
    worker.finished_at = None;
    worker.awaiting_started_at = None;
    worker.awaiting_since = None;
    worker.latest_error = None;
    if clear_latest_payload {
        worker.latest_payload = None;
    }
}

pub(super) fn worker_replay_carry(worker: &WorkerState) -> Result<WorkerReplayCarry, VmError> {
    Ok(WorkerReplayCarry {
        artifacts: apply_worker_artifact_policy(&worker.artifacts, &worker.carry_policy),
        transcript: apply_worker_transcript_policy(
            worker.transcript.clone(),
            &worker.carry_policy,
        )?,
        parent_worker_id: worker.id.clone(),
        worker_id: worker.id.clone(),
        resume_workflow: worker.carry_policy.resume_workflow,
        child_run_path: worker.child_run_path.clone(),
        reset_sub_agent_session: matches!(
            worker.carry_policy.transcript_mode.as_str(),
            "fork" | "reset"
        ),
    })
}

pub(super) fn apply_worker_replay_config(
    config: &mut WorkerConfig,
    next_task: &str,
    carry: WorkerReplayCarry,
) {
    match config {
        WorkerConfig::Workflow {
            artifacts, options, ..
        } => apply_workflow_replay_config(artifacts, options, carry),
        WorkerConfig::Stage {
            artifacts,
            transcript,
            ..
        } => apply_stage_replay_config(artifacts, transcript, carry),
        WorkerConfig::SubAgent { spec } => {
            apply_sub_agent_replay_config(spec, next_task, carry.reset_sub_agent_session);
        }
    }
}

pub(super) fn apply_workflow_replay_config(
    artifacts: &mut Vec<ArtifactRecord>,
    options: &mut crate::value::DictMap,
    carry: WorkerReplayCarry,
) {
    if !carry.artifacts.is_empty() {
        *artifacts = carry.artifacts;
    }
    options.put_str("parent_worker_id", carry.parent_worker_id);
    if let Some(transcript) = carry.transcript {
        options.insert(crate::value::intern_key("transcript"), transcript);
    } else {
        options.remove("transcript");
    }
    if carry.resume_workflow {
        if let Some(child_run_path) = carry.child_run_path {
            options.put_str("resume_path", child_run_path);
        }
    } else {
        options.remove("resume_path");
    }
}

pub(super) fn apply_stage_replay_config(
    artifacts: &mut Vec<ArtifactRecord>,
    transcript: &mut Option<VmValue>,
    carry: WorkerReplayCarry,
) {
    if !carry.artifacts.is_empty() {
        *artifacts = carry.artifacts;
    }
    *transcript = carry.transcript;
}

pub(super) fn apply_sub_agent_replay_config(
    spec: &mut SubAgentRunSpec,
    next_task: &str,
    reset_session: bool,
) {
    spec.task = next_task.to_string();
    if reset_session {
        spec.session_id = format!("sub_agent_session_{}", uuid::Uuid::now_v7());
        spec.options.put_str("session_id", spec.session_id.clone());
    }
}

pub(super) fn respawn_worker_task(
    state: Arc<parking_lot::Mutex<WorkerState>>,
    ctx: &AsyncBuiltinCtx,
) -> Result<(), VmError> {
    {
        let worker = state.lock();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    spawn_worker_task(state, ctx.child_ctx());
    Ok(())
}

pub(super) async fn wait_for_worker_terminal(
    state: Arc<parking_lot::Mutex<WorkerState>>,
    context: &str,
) -> Result<(), VmError> {
    loop {
        let handle = state.lock().handle.take();
        if let Some(handle) = handle {
            let _ = handle
                .await
                .map_err(|error| VmError::Runtime(format!("{context} join error: {error}")))??;
            continue;
        }
        if !worker_wait_blocks(&state.lock().status) {
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
