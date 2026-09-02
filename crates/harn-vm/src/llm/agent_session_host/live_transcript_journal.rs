//! Canonical transcript lifecycle for a live Harn agent session.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{DictMap, VmError, VmValue};
use crate::vm::Vm;

const HOST_SESSION_FLUSH: &str = "__host_agent_session_flush";
const HOST_AGENT_EMIT_EVENT: &str = "__host_agent_emit_event";

pub(super) struct InitializedSession {
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) has_canonical_history: bool,
}

/// Configure canonical persistence before hooks run. A cold session with
/// durable history is seeded before the hook so its context is available to
/// the rest of initialization; a live session keeps its VM state.
pub(super) async fn initialize(
    session_id: &str,
    options: &DictMap,
    system_prompt: Option<String>,
) -> Result<InitializedSession, VmError> {
    let has_live_session = crate::agent_sessions::exists(session_id);
    let run_id = options
        .get("run_id")
        .and_then(|value| match value {
            VmValue::String(value) if !value.trim().is_empty() => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| format!("agent_run_{}", uuid::Uuid::now_v7()));
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        options,
        run_id.clone(),
        format!("agent_loop_{}", uuid::Uuid::now_v7()),
    )
    .await?;
    let has_canonical_history =
        !prepared.transcript.messages.is_empty() || !prepared.transcript.events.is_empty();
    let session_id = if has_canonical_history && !has_live_session {
        let seeded_session_id = crate::agent_sessions::seed_from_messages_and_events(
            Some(session_id.to_string()),
            &prepared.transcript.messages,
            Some(&prepared.transcript.events),
            serde_json::json!({}),
            system_prompt,
            None,
        )
        .map_err(VmError::Runtime)?;
        crate::agent_sessions::restore_message_event_ids(
            &seeded_session_id,
            &prepared.transcript.source_event_ids,
        )
        .map_err(VmError::Runtime)?;
        seeded_session_id
    } else {
        crate::agent_sessions::open_or_create(Some(session_id.to_string()))
    };
    crate::agent_sessions::install_journal(&session_id, prepared.state)?;
    if let Err(error) = stamp_run_started(&session_id).await {
        crate::agent_sessions::clear_journal(&session_id);
        return Err(error);
    }
    Ok(InitializedSession {
        session_id,
        run_id,
        has_canonical_history,
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
    let event = super::super::helpers::transcript_event(
        "agent_run_started",
        "system",
        "internal",
        "Agent loop started",
        Some(serde_json::json!({
            "execution_id": execution_id,
            "lifecycle_state": crate::agent_events::AgentLifecycleState::Running.wire_name(),
        })),
    );
    crate::agent_sessions::append_journal_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await
}

pub(super) async fn flush_init_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
) -> Result<(), VmError> {
    let terminal = crate::agent_events::AgentTerminalOutcome::new(
        crate::agent_events::classify_agent_terminal(final_status, stop_reason, false, None),
        stop_reason,
    );
    let event = super::super::helpers::transcript_event(
        "agent_run_terminal",
        "assistant",
        "internal",
        "Agent loop did not enter its provider phase",
        Some(serde_json::json!({
            "final_status": final_status,
            "stop_reason": stop_reason,
            "terminal": terminal,
        })),
    );
    crate::agent_sessions::append_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await?;
    crate::agent_sessions::clear_journal(session_id);
    Ok(())
}

pub(super) async fn flush_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    terminal_class: Option<&str>,
    terminal_error: Option<&serde_json::Value>,
    terminal: &crate::agent_events::AgentTerminalOutcome,
) -> Result<(), VmError> {
    let event = super::super::helpers::transcript_event(
        "agent_run_terminal",
        "assistant",
        "internal",
        "Agent loop reached a terminal state",
        Some(serde_json::json!({
            "final_status": final_status,
            "stop_reason": stop_reason,
            "terminal_class": terminal_class,
            "error": terminal_error,
            "terminal": terminal,
        })),
    );
    crate::agent_sessions::append_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await?;
    crate::agent_sessions::clear_journal(session_id);
    Ok(())
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
    let payload = super::vm_to_json(&payload_value);
    if event_type == "model_job" {
        crate::testbench::tape::record_model_job_event(&payload);
    }
    let Some(event) =
        crate::agent_events::AgentEvent::from_host_payload(&session_id, &event_type, &payload)?
    else {
        return Ok(VmValue::Nil);
    };
    if let Some(role) = crate::agent_events::AgentEvent::host_transcript_role(event_type.as_str()) {
        let transcript_event = super::super::helpers::transcript_event(
            &event_type,
            role.as_str(),
            "internal",
            "",
            Some(payload),
        );
        if crate::agent_sessions::exists(&session_id) {
            crate::agent_sessions::append_event(&session_id, transcript_event)
                .map_err(VmError::Runtime)?;
        }
    }
    crate::llm::agent_runtime::emit_agent_event_with_ctx(Some(&ctx), &event).await;
    Ok(VmValue::Nil)
}

const LIVE_TRANSCRIPT_JOURNAL_BUILTINS: &[&VmBuiltinDef] =
    &[&HOST_AGENT_SESSION_FLUSH_DEF, &HOST_AGENT_EMIT_EVENT_DEF];

pub(super) fn register_live_transcript_journal_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, LIVE_TRANSCRIPT_JOURNAL_BUILTINS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    #[tokio::test]
    async fn cold_initialize_restores_non_message_typed_checkpoints() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let mut options = DictMap::new();
        options.put_str("root", root.path().to_string_lossy().as_ref());
        let session_id = "cold-typed-checkpoint-rehydration";

        initialize(session_id, &options, None)
            .await
            .expect("initialize first run");
        crate::agent_sessions::inject_message(
            session_id,
            crate::stdlib::json_to_vm_value(
                &serde_json::json!({"role": "user", "content": "persist the run"}),
            ),
        )
        .expect("inject canonical message");
        let checkpoint = super::super::helpers::transcript_event(
            "typed_checkpoint",
            "system",
            "internal",
            "",
            Some(serde_json::json!({
                "schema": "harn.goal_transition.v1",
                "message_id": "steer-cold-1",
            })),
        );
        crate::agent_sessions::append_event(session_id, checkpoint)
            .expect("append typed checkpoint");
        let durable_goal_pin = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "id": "goal-pin-bravo",
            "kind": "system_reminder",
            "role": "system",
            "visibility": "public",
            "content": "Goal: BRAVO",
            "reminder": {
                "dedupe_key": "pin/goal",
                "preserve_on_compact": true,
                "body": "Goal: BRAVO"
            }
        }));
        crate::agent_sessions::append_event(session_id, durable_goal_pin)
            .expect("append durable goal pin");
        let transient_old_goal = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "id": "transient-goal-alpha",
            "kind": "system_reminder",
            "role": "system",
            "visibility": "public",
            "content": "Goal: ALPHA",
            "reminder": {
                "dedupe_key": "transient/old-goal",
                "preserve_on_compact": false,
                "body": "Goal: ALPHA"
            }
        }));
        crate::agent_sessions::append_event(session_id, transient_old_goal)
            .expect("append transient old goal");
        crate::agent_sessions::replace_messages_with_summary(
            session_id,
            &[serde_json::json!({"role": "user", "content": "compacted history"})],
            Some("compacted history"),
        )
        .expect("compact live transcript");
        crate::agent_session_journal::flush(session_id)
            .await
            .expect("flush first run");
        crate::agent_sessions::reset_session_store();

        let rehydrated = initialize(session_id, &options, None)
            .await
            .expect("cold initialize second run");
        assert!(rehydrated.has_canonical_history);
        let transcript = crate::agent_sessions::transcript(session_id).expect("rehydrated session");
        let events = transcript
            .as_dict()
            .and_then(|transcript| transcript.get("events"))
            .and_then(|events| match events {
                VmValue::List(events) => Some(events),
                _ => None,
            })
            .expect("rehydrated events");
        let receipts = events
            .iter()
            .filter(|event| {
                event
                    .as_dict()
                    .and_then(|event| event.get("metadata"))
                    .and_then(VmValue::as_dict)
                    .and_then(|metadata| metadata.get("schema"))
                    .map(VmValue::display)
                    .as_deref()
                    == Some("harn.goal_transition.v1")
            })
            .count();
        assert_eq!(receipts, 1, "cold hydration must not drop typed receipts");
        let goal_pins = events
            .iter()
            .filter(|event| {
                event
                    .as_dict()
                    .and_then(|event| event.get("reminder"))
                    .and_then(VmValue::as_dict)
                    .and_then(|reminder| reminder.get("dedupe_key"))
                    .map(VmValue::display)
                    .as_deref()
                    == Some("pin/goal")
            })
            .collect::<Vec<_>>();
        assert_eq!(goal_pins.len(), 1, "durable managed goal pin must survive");
        assert!(
            goal_pins[0].display().contains("BRAVO"),
            "retargeted goal pin must retain the current goal"
        );
        assert!(
            events
                .iter()
                .all(|event| !event.display().contains("transient-goal-alpha")),
            "non-durable reminders must still be removed by compaction"
        );
        crate::agent_sessions::reset_session_store();
    }
}
