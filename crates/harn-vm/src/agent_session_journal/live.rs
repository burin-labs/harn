//! Canonical transcript lifecycle for a live Harn agent session.

use crate::value::{DictMap, VmError, VmValue};
use crate::vm::Vm;
use harn_builtin_macros::harn_builtin;
use harn_session_store::SessionStore;

const HOST_SESSION_FLUSH: &str = "__host_agent_session_flush";
const HOST_AGENT_EMIT_EVENT: &str = "__host_agent_emit_event";

pub(crate) struct InitializedSession {
    _session_scope: crate::agent_sessions::CurrentSessionGuard,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) has_canonical_history: bool,
    /// True when this initialization created the VM session rather than
    /// borrowing an already-live caller session with the same public id.
    pub(crate) owns_session: bool,
}

/// Synchronous cancellation fallback for the await between journal install
/// and returning the acquisition receipt to the outer lifecycle owner.
struct JournalInitializationRollback {
    armed: bool,
}

impl JournalInitializationRollback {
    fn new() -> Self {
        Self { armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn preserve_for_task_cleanup(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
    }
}

impl Drop for JournalInitializationRollback {
    fn drop(&mut self) {
        // Hard cancellation cannot await SQLite. The journal's task claim and
        // writer lease remain live for the cancelling VM task to terminalize.
        self.preserve_for_task_cleanup();
    }
}

/// Configure canonical persistence before hooks run. A cold session with
/// durable history is seeded before the hook so its context is available to
/// the rest of initialization; a live session keeps its VM state.
pub(crate) async fn initialize(
    session_id: &str,
    options: &DictMap,
    system_prompt: Option<String>,
    execution_id: String,
    task_id: String,
    ctx: &crate::vm::AsyncBuiltinCtx,
) -> Result<InitializedSession, VmError> {
    if let Some((run_id, has_canonical_history, owns_session)) =
        super::adopt_prepared_run(session_id, &execution_id, &task_id, options)?
    {
        return Ok(InitializedSession {
            _session_scope: crate::agent_sessions::enter_current_session(session_id),
            session_id: session_id.to_string(),
            run_id,
            has_canonical_history,
            owns_session,
        });
    }
    let has_live_session = crate::agent_sessions::exists(session_id);
    let pending_fork_goal = crate::agent_sessions::has_pending_fork_goal(session_id);
    let mut preparation_options = options.clone();
    let fork_parent = pending_fork_goal
        .then(|| crate::agent_sessions::parent_id(session_id))
        .flatten();
    if let Some(parent) = &fork_parent {
        use crate::value::VmDictExt;
        preparation_options.put_str("parent_session_id", parent);
    }
    let run_id = options
        .get("run_id")
        .and_then(|value| match value {
            VmValue::String(value) if !value.trim().is_empty() => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| format!("agent_run_{}", uuid::Uuid::now_v7()));
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &preparation_options,
        run_id.clone(),
        format!("agent_loop_{}", uuid::Uuid::now_v7()),
    )
    .await?;
    prepared
        .transcript
        .validate_goal()
        .map_err(VmError::Runtime)?;
    if pending_fork_goal {
        let metadata = prepared
            .state
            .config
            .store
            .describe(session_id)
            .await
            .map_err(|error| VmError::Runtime(format!("fork goal admission: {error}")))?;
        if prepared.has_events || fork_parent.is_none() || metadata.parent_session_id != fork_parent
        {
            return Err(VmError::Runtime(
                "fork goal admission conflicts with canonical history or lineage; use a new destination or a canonical fork".into(),
            ));
        }
    }
    let has_canonical_history = !prepared.transcript.messages.is_empty();
    let session_id = if has_canonical_history && !has_live_session {
        let seeded_session_id = crate::agent_sessions::seed_from_messages(
            Some(session_id.to_string()),
            &prepared.transcript.messages,
            serde_json::json!({}),
            system_prompt,
            None,
        )
        .map_err(VmError::Runtime)?;
        if let Err(error) = crate::agent_sessions::restore_message_event_ids(
            &seeded_session_id,
            &prepared.transcript.source_event_ids,
        ) {
            crate::agent_sessions::close(&seeded_session_id);
            return Err(VmError::Runtime(error));
        }
        seeded_session_id
    } else {
        crate::agent_sessions::open_or_create(Some(session_id.to_string()))
            .map_err(|error| VmError::Runtime(error.to_string()))?
    };
    let owns_session = !has_live_session;
    crate::agent_sessions::install_journal(&session_id, prepared.state)?;
    if let Err(error) =
        crate::agent_sessions::claim_journal_task(&session_id, &execution_id, task_id, owns_session)
    {
        crate::agent_sessions::clear_journal(&session_id);
        if owns_session {
            crate::agent_sessions::close(&session_id);
        }
        return Err(error);
    }
    // Hydration must follow writer ownership: a refused competing actor must
    // not replace an active session's goal from another canonical root.
    let goal_ready = if pending_fork_goal {
        crate::agent_sessions::persist_pending_fork_goal(&session_id)
    } else {
        prepared.transcript.restore_goal(&session_id)
    };
    if let Err(error) = goal_ready {
        crate::agent_sessions::clear_journal(&session_id);
        if owns_session {
            crate::agent_sessions::close(&session_id);
        }
        return Err(VmError::Runtime(error));
    }
    // Installation is synchronous, so the ownership guard can begin here
    // without a cancellation gap. Starting it before install would let an
    // "already active" error clear a journal owned by the caller.
    let mut rollback = JournalInitializationRollback::new();
    if let Err(error) = stamp_run_started(&session_id).await {
        // This is an ordinary returned error, so complete the terminal write
        // here. If that write also fails, preserve the claimed journal and
        // lease for an explicit cancellation/shutdown retry.
        if flush_init_terminal_with_cause(
            &session_id,
            "failed",
            "session_initialization_failed",
            Some(ctx),
            Some(&error),
        )
        .await
        .is_ok()
        {
            rollback.disarm();
            if owns_session {
                crate::agent_sessions::close(&session_id);
            }
        }
        return Err(error);
    }
    rollback.disarm();
    Ok(InitializedSession {
        _session_scope: crate::agent_sessions::enter_current_session(session_id.clone()),
        session_id,
        run_id,
        has_canonical_history,
        owns_session,
    })
}

/// Persist the run boundary before hooks, policy checks, or provider work.
///
/// The journal owns both this start stamp and the terminal stamp below. Run
/// projections therefore read one durable clock instead of deriving elapsed
/// time from mutable session-row metadata.
async fn stamp_run_started(session_id: &str) -> Result<(), VmError> {
    // Normal VM calls inherit the invocation identity. Direct host and nested
    // session entry points are also valid, so give those runs a VM-owned
    // identity instead of aborting the agent loop or persisting no identity.
    let execution_id = crate::current_execution_scope().unwrap_or_else(crate::mint_execution_scope);
    let event = crate::llm::helpers::transcript_event(
        "agent_run_started",
        "system",
        "internal",
        "Agent loop started",
        Some(serde_json::json!({
            "execution_id": execution_id,
            "reasoning_receipts_reported": true,
            "lifecycle_state": crate::agent_events::AgentLifecycleState::Running.wire_name(),
        })),
    );
    crate::agent_sessions::append_journal_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await
}

pub(crate) async fn flush_init_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
) -> Result<crate::llm::agent_session_host::PhysicalUsageSnapshot, VmError> {
    flush_init_terminal_with_cause(session_id, final_status, stop_reason, ctx, None).await
}

fn init_failure_cause(cause: &VmError) -> serde_json::Value {
    const MAX_CAUSE_BYTES: usize = 2048;
    let rendered = cause.to_string();
    let mut message = crate::redact::current_policy()
        .redact_string(&rendered)
        .into_owned();
    if message.len() > MAX_CAUSE_BYTES {
        let mut end = MAX_CAUSE_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    serde_json::json!({
        "category": crate::value::error_to_category(cause).as_str(),
        "kind": if matches!(cause, VmError::UndefinedBuiltin(_)) { "undefined_builtin" } else { "vm_error" },
        "message": message,
    })
}

#[cfg(test)]
mod init_failure_cause_tests {
    use super::{init_failure_cause, VmError};

    #[test]
    fn cause_is_redacted_before_persistence_and_bounded_on_utf8_boundary() {
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let cause = VmError::Runtime(format!("{secret} {}", "é".repeat(2048)));
        assert!(
            cause.to_string().contains(secret),
            "positive raw-cause control"
        );
        let value = init_failure_cause(&cause);
        let message = value["message"].as_str().expect("cause message");
        assert!(!message.contains(secret));
        assert!(message.contains("<redacted:openai_key:"));
        assert!(message.len() <= 2048);
        assert!(message.len() > 2000, "the limit must actually be reached");
    }
}

pub(crate) async fn flush_init_terminal_with_cause(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    cause: Option<&VmError>,
) -> Result<crate::llm::agent_session_host::PhysicalUsageSnapshot, VmError> {
    let physical_usage =
        crate::llm::agent_session_host::physical_usage_snapshot_for_session(session_id)
            .ok_or_else(|| VmError::Runtime("agent session has no physical usage owner".into()))?;
    let error = cause.map(init_failure_cause);
    let terminal = crate::agent_events::AgentTerminalOutcome::new(
        crate::agent_events::classify_agent_terminal(final_status, stop_reason, false, None),
        stop_reason,
    )
    .with_error(error.as_ref());
    let event = crate::llm::helpers::transcript_event(
        "agent_run_terminal",
        "assistant",
        "internal",
        "Agent run ended before its loop started",
        Some(serde_json::json!({
            "final_status": final_status,
            "stop_reason": stop_reason,
            "terminal": terminal,
            "error": error,
            "visible_reply": "",
            "provider_call_count": physical_usage.dispatched_calls,
            "goal_outcome": serde_json::Value::Null,
        })),
    );
    let phase = append_terminal_phase(session_id, event)?;
    crate::agent_session_journal::flush(session_id).await?;
    // Subscriber delivery can re-enter the VM. Keep its future off the
    // initialization caller's stack.
    Box::pin(crate::llm::emit_live_agent_event_with_ctx(
        ctx,
        &crate::agent_events::AgentEvent::TurnPhaseChanged {
            session_id: session_id.to_string(),
            phase,
        },
    ))
    .await;
    crate::agent_sessions::clear_journal(session_id);
    Ok(physical_usage)
}

pub(crate) struct TerminalAccounting<'a> {
    pub provider_call_count: i64,
    /// The loop owns the decision schema; the host persists its account unchanged.
    pub adaptive_budget: Option<&'a serde_json::Value>,
    /// The completion owner supplies the assessment; persistence does not rejudge it.
    pub goal_outcome: Option<&'a serde_json::Value>,
}

pub(crate) async fn flush_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    terminal_class: Option<&str>,
    terminal_error: Option<&serde_json::Value>,
    terminal: &crate::agent_events::AgentTerminalOutcome,
    accounting: TerminalAccounting<'_>,
) -> Result<crate::agent_events::AgentTurnPhase, VmError> {
    let visible_reply = crate::agent_sessions::transcript(session_id)
        .as_ref()
        .and_then(crate::llm::agent_result_projection::last_assistant_text)
        .unwrap_or_default();
    let mut metadata = serde_json::json!({
        "final_status": final_status,
        "stop_reason": stop_reason,
        "terminal_class": terminal_class,
        "error": terminal_error,
        "terminal": terminal,
        "visible_reply": visible_reply,
        "provider_call_count": accounting.provider_call_count,
        "goal_outcome": accounting.goal_outcome,
    });
    if let Some(budget) = accounting.adaptive_budget {
        metadata["adaptive_budget"] = budget.clone();
    }
    let event = crate::llm::helpers::transcript_event(
        "agent_run_terminal",
        "assistant",
        "internal",
        "Agent loop reached a terminal state",
        Some(metadata),
    );
    let phase = append_terminal_phase(session_id, event)?;
    crate::agent_session_journal::flush(session_id).await?;
    Ok(phase)
}

fn append_terminal_phase(
    session_id: &str,
    event: VmValue,
) -> Result<crate::agent_events::AgentTurnPhase, VmError> {
    let record = crate::agent_sessions::append_terminal_event_once(session_id, event)
        .map_err(VmError::Runtime)?;
    let record = crate::llm::helpers::vm_value_to_json(&record);
    record
        .get("metadata")
        .and_then(crate::agent_events::AgentTurnPhase::from_terminal_record)
        .ok_or_else(|| {
            VmError::Runtime("agent terminal record lacks its typed reply or outcome".into())
        })
}

/// Flush the live transcript journal at the common pre-provider boundary.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_flush(session_id: string) -> nil",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_flush(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_FLUSH}: session_id must be a non-empty string"
        )));
    }
    crate::agent_session_journal::flush(&session_id).await?;
    Ok(VmValue::Nil)
}

/// Emit an agent event and persist transcript-backed lifecycle types.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_emit_event(session_id: string, event_type: string, payload: dict) -> nil",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_emit_event(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_EMIT_EVENT}: session_id must be a non-empty string"
            )))
        }
    };
    let event_type = match args.get(1) {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_EMIT_EVENT}: event_type must be a non-empty string"
            )))
        }
    };
    // A new agent-loop iteration begins: drop the per-turn host-capability
    // memo so turn-stable facts (e.g. `runtime.pipeline_input`, which the host
    // re-projects each turn for mid-session model switches) are re-read exactly
    // once this turn instead of being served stale from the prior turn. This is
    // the canonical turn boundary for both the harn agent loop and embedder
    // loops that drive it. harn#5190.
    if event_type == "iteration_start" {
        crate::stdlib::host::turn_cache::reset();
    }
    let payload_value = args.get(2).cloned().unwrap_or(VmValue::Nil);
    let payload = crate::llm::vm_value_to_json(&payload_value);
    if event_type == "model_job" {
        crate::testbench::tape::record_model_job_event(&payload);
    }
    let Some(event) =
        crate::agent_events::AgentEvent::from_host_payload(&session_id, &event_type, &payload)?
    else {
        return Ok(VmValue::Nil);
    };
    event
        .publish_host_event(&ctx, &session_id, &event_type, payload)
        .await?;
    Ok(VmValue::Nil)
}

/// Observe a tool outcome through the existing stdlib verification owner.
/// Generic event ingress cannot supply its own normalized health measurement.
#[harn_builtin(
    exposure = "harness.agent.emit_tool_outcome",
    effects = [],
    sig = "__host_agent_emit_tool_outcome(session_id: string, payload: dict, envelope: dict, result: dict) -> nil",
    kind = "async",
    category = "agent.host"
)]
async fn host_agent_emit_tool_outcome(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => {
            return Err(VmError::Runtime(
                "tool outcome requires a session_id".into(),
            ))
        }
    };
    let mut payload = crate::llm::vm_value_to_json(args.get(1).unwrap_or(&VmValue::Nil));
    let Some(mut event) = crate::agent_events::AgentEvent::from_host_payload(
        &session_id,
        "tool_call_update",
        &payload,
    )?
    else {
        return Err(VmError::Runtime(
            "tool outcome event was not decoded".into(),
        ));
    };
    let mut vm = ctx.child_vm();
    let agent = vm
        .harness()
        .and_then(|root| root.sub_handle("agent"))
        .map(VmValue::harness)
        .ok_or_else(|| {
            VmError::Runtime("tool outcome requires the execution's agent capability".into())
        })?;
    let exports = vm
        .load_module_exports_from_import("std/agent/session_health")
        .await?;
    let normalizer = exports
        .get("__agent_tool_health_telemetry")
        .ok_or_else(|| VmError::Runtime("stdlib tool outcome normalizer is not exported".into()))?;
    let normalized = vm
        .call_closure_pub(
            normalizer,
            &[
                agent,
                args.get(2).cloned().unwrap_or(VmValue::Nil),
                args.get(3).cloned().unwrap_or(VmValue::Nil),
            ],
        )
        .await?;
    let measurement = crate::llm::vm_value_to_json(&normalized);
    event.attach_normalized_tool_health(measurement.clone())?;
    // The canonical journal keeps the same normalized measurement as the live
    // event. It is added only after rejecting generic payload-supplied health.
    payload["health"] = measurement;
    event
        .publish_host_event(&ctx, &session_id, "tool_call_update", payload)
        .await?;
    Ok(VmValue::Nil)
}

pub(crate) fn register_live_transcript_journal_primitives(vm: &mut Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}
