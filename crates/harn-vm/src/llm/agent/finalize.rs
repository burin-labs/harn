//! Post-loop finalize phase.
//!
//! After `run_agent_loop_internal`'s iteration `for`-loop exits (via
//! `break`, `continue`-to-budget, or falling off the end), control
//! flows here to drain the deferred-user queue, write a final daemon
//! snapshot, emit the structured trace completion event, and build the
//! result dict the caller sees.
//!
//! Lives in its own module for the same reason the prelude lives on
//! `AgentLoopState::new` — it has no control-flow interaction with
//! the iteration body and benefits from being isolated so the
//! orchestrator in `mod.rs` reads as a short sequence of phases.

use std::rc::Rc;
use std::time::Instant;

use crate::value::VmError;

use super::super::helpers::transcript_to_vm_with_events;
use super::helpers::{
    daemon_snapshot_from_state, inject_queued_user_messages, maybe_persist_daemon_snapshot,
};
use super::state::AgentLoopState;

/// Run the finalize phase after the iteration loop has exited.
///
/// Reads config fields it needs (`require_successful_tools`) from
/// `state.config` directly rather than taking `&AgentLoopConfig` as a
/// separate parameter, because the call site already holds a
/// `&state.config` immutable borrow that would conflict with the
/// `&mut state` receiver here.
pub(super) async fn run_finalize(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    bridge: Option<Rc<crate::bridge::HostBridge>>,
    daemon: bool,
    daemon_config: &super::super::daemon::DaemonLoopConfig,
    tool_format: &str,
    loop_start: Instant,
) -> Result<serde_json::Value, VmError> {
    state.deferred_user_messages.extend(
        inject_queued_user_messages(
            bridge.as_ref(),
            &mut state.visible_messages,
            crate::bridge::DeliveryCheckpoint::EndOfInteraction,
        )
        .await?
        .into_iter()
        .map(|message| message.content),
    );

    if daemon && state.final_status == "done" {
        state.final_status = "idle";
    }
    // Capture required-tools list before mutating state so the
    // immutable borrow on state.config drops before we reassign final_status.
    let required_tools_failed = if state.final_status == "done" {
        match state.config.require_successful_tools.as_ref() {
            Some(required_tools) if !required_tools.is_empty() => !state
                .successful_tools_used
                .iter()
                .any(|tool_name| required_tools.iter().any(|wanted| wanted == tool_name)),
            _ => false,
        }
    } else {
        false
    };
    if required_tools_failed {
        state.final_status = "failed";
    }
    if daemon {
        state.daemon_state = state.final_status.to_string();
        let final_snapshot = daemon_snapshot_from_state(
            &state.daemon_state,
            &state.visible_messages,
            &state.recorded_messages,
            &state.transcript_summary,
            &state.transcript_events,
            &state.total_text,
            &state.last_iteration_text,
            &state.all_tools_used,
            &state.rejected_tools,
            &state.deferred_user_messages,
            state.total_iterations,
            state.idle_backoff_ms,
            state.last_run_exit_code,
            &state.daemon_watch_state,
        );
        state.daemon_snapshot_path = maybe_persist_daemon_snapshot(daemon_config, &final_snapshot)?
            .or(state.daemon_snapshot_path.take());
    }

    crate::llm::trace::emit_agent_event(crate::llm::trace::AgentTraceEvent::LoopComplete {
        status: state.final_status.to_string(),
        iterations: state.total_iterations,
        total_duration_ms: loop_start.elapsed().as_millis() as u64,
        tools_used: state.all_tools_used.clone(),
        successful_tools: state.successful_tools_used.clone(),
    });
    let trace_summary = crate::llm::trace::agent_trace_summary();

    // Expose final ledger state so post-processors (QC officer, audit
    // pipelines) can reason over what the agent considered "done".
    let ledger_json = serde_json::to_value(&state.task_ledger).unwrap_or(serde_json::Value::Null);
    let ledger_done_nudge_count = state.ledger_done_rejections as i64;

    let _ = opts;
    let transcript_vm = transcript_to_vm_with_events(
        Some(state.session_id.clone()),
        state.transcript_summary.clone(),
        None,
        &state.recorded_messages,
        state.transcript_events.clone(),
        Vec::new(),
        Some(if state.final_status == "done" {
            "active"
        } else {
            "paused"
        }),
    );
    if !state.anonymous_session {
        crate::agent_sessions::store_transcript(&state.session_id, transcript_vm.clone());
        // Persist the active-skill set so a re-entry of this session
        // resumes the same scope without re-matching from scratch.
        crate::agent_sessions::set_active_skills(
            &state.session_id,
            state.active_skills.iter().map(|s| s.name.clone()).collect(),
        );
    }
    let transcript_json = crate::llm::helpers::vm_value_to_json(&transcript_vm);

    Ok(serde_json::json!({
        "status": state.final_status,
        "daemon_state": state.daemon_state,
        "daemon_snapshot_path": state.daemon_snapshot_path,
        "text": state.total_text,
        "visible_text": state.last_iteration_text,
        "iterations": state.total_iterations,
        "duration_ms": loop_start.elapsed().as_millis() as i64,
        "tools_used": state.all_tools_used,
        "successful_tools": state.successful_tools_used,
        "rejected_tools": state.rejected_tools,
        "tool_calling_mode": tool_format,
        "deferred_user_messages": state.deferred_user_messages,
        "task_ledger": ledger_json,
        "ledger_done_rejections": ledger_done_nudge_count,
        "trace": trace_summary,
        "transcript": transcript_json,
    }))
}
