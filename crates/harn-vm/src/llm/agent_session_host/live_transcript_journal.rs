//! Canonical transcript lifecycle for a live Harn agent session.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{DictMap, VmError, VmValue};
use crate::vm::Vm;

const HOST_SESSION_FLUSH: &str = "__host_agent_session_flush";

pub(super) struct InitializedSession {
    pub(super) session_id: String,
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
    let hydrated = crate::agent_session_journal::hydrate_and_configure(
        session_id,
        options,
        format!("agent_run_{}", uuid::Uuid::now_v7()),
        format!("agent_turn_{}", uuid::Uuid::now_v7()),
    )
    .await?;
    let has_canonical_history = !hydrated.messages.is_empty();
    let session_id = if has_canonical_history && !has_live_session {
        let seeded_session_id = crate::agent_sessions::seed_from_messages(
            Some(session_id.to_string()),
            &hydrated.messages,
            serde_json::json!({}),
            system_prompt,
            None,
        )
        .map_err(VmError::Runtime)?;
        crate::agent_sessions::restore_message_event_ids(
            &seeded_session_id,
            &hydrated.source_event_ids,
        )
        .map_err(VmError::Runtime)?;
        seeded_session_id
    } else {
        crate::agent_sessions::open_or_create(Some(session_id.to_string()))
    };
    Ok(InitializedSession {
        session_id,
        has_canonical_history,
    })
}

pub(super) async fn flush_init_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
) -> Result<(), VmError> {
    let event = super::super::helpers::transcript_event(
        "agent_run_terminal",
        "assistant",
        "internal",
        "Agent loop did not enter its provider phase",
        Some(serde_json::json!({
            "final_status": final_status,
            "stop_reason": stop_reason,
        })),
    );
    crate::agent_sessions::append_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await?;
    crate::agent_session_journal::clear(session_id);
    Ok(())
}

pub(super) async fn flush_terminal(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    terminal_class: Option<&str>,
    terminal_error: Option<&serde_json::Value>,
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
        })),
    );
    crate::agent_sessions::append_event(session_id, event).map_err(VmError::Runtime)?;
    crate::agent_session_journal::flush(session_id).await?;
    crate::agent_session_journal::clear(session_id);
    Ok(())
}

/// Flush the live transcript journal at the common pre-provider boundary.
#[harn_builtin(
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

const LIVE_TRANSCRIPT_JOURNAL_BUILTINS: &[&VmBuiltinDef] = &[&HOST_AGENT_SESSION_FLUSH_DEF];

pub(super) fn register_live_transcript_journal_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, LIVE_TRANSCRIPT_JOURNAL_BUILTINS);
}
