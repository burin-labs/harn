//! Turn-preflight phase.
//!
//! Runs at the head of every iteration of the turn loop, BEFORE the
//! LLM is called:
//!
//!   1. Drain any host-initiated user messages pushed via the bridge
//!      queue into `state.visible_messages` / `state.recorded_messages`.
//!   2. Build the composite system prompt (base + tool contract +
//!      persistent system).
//!   3. Emit `AgentEvent::TurnStart` so pipeline subscribers can set
//!      up per-turn state.
//!   4. Drain the pending-feedback queue (items pushed by subscribers
//!      via `agent_inject_feedback(session_id, …)`) and inject each
//!      as a runtime-feedback message. Emit a `FeedbackInjected`
//!      event per item.
//!   5. Render the task ledger as a transient user message at the
//!      tail of the call payload (not added to visible/recorded
//!      history).
//!   6. Write the assembled `call_messages` / `call_system` into
//!      `opts` so the LLM call phase picks them up.

use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::value::VmError;

use super::super::helpers::transcript_event;
use super::helpers::{
    append_host_messages_to_recorded, append_message_to_contexts, build_agent_system_prompt,
    inject_queued_user_messages, runtime_feedback_message,
};
use super::state::AgentLoopState;
use super::{drain_pending_feedback, emit_agent_event};

/// Carry-over context the preflight phase needs from `run_agent_loop_internal`
/// but that isn't otherwise on `AgentLoopState` (either because it's an
/// immutable config snapshot or a borrowed handle the phase doesn't own).
pub(super) struct PreflightContext<'a> {
    pub bridge: &'a Option<Rc<crate::bridge::HostBridge>>,
    pub session_id: &'a str,
    pub resumed_iterations: usize,
    pub iteration: usize,
    pub base_system: Option<&'a str>,
    pub tool_contract_prompt: Option<&'a str>,
    pub persistent_system_prompt: Option<&'a str>,
}

pub(super) async fn run_turn_preflight(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: PreflightContext<'_>,
) -> Result<(), VmError> {
    state.total_iterations = ctx.resumed_iterations + ctx.iteration + 1;
    crate::llm::agent_observe::set_current_iteration(Some(state.total_iterations));
    state.daemon_state = "active".to_string();

    let immediate_messages = inject_queued_user_messages(
        ctx.bridge.as_ref(),
        &mut state.visible_messages,
        crate::bridge::DeliveryCheckpoint::InterruptImmediate,
    )
    .await?;
    append_host_messages_to_recorded(&mut state.recorded_messages, &immediate_messages);
    for message in &immediate_messages {
        state.transcript_events.push(transcript_event(
            "host_input",
            "user",
            "public",
            &message.content,
            Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
        ));
    }
    if !immediate_messages.is_empty() {
        state.consecutive_text_only = 0;
        state.idle_backoff_ms = 100;
    }

    let default_system = build_agent_system_prompt(
        ctx.base_system,
        ctx.tool_contract_prompt,
        ctx.persistent_system_prompt,
    );
    let mut call_messages = state.visible_messages.clone();
    let call_system = default_system;

    // Emit TurnStart before draining pending feedback so subscribers
    // see the boundary before any drain-generated injections land.
    emit_agent_event(&AgentEvent::TurnStart {
        session_id: ctx.session_id.to_string(),
        iteration: ctx.iteration,
    })
    .await;

    for (kind, content) in drain_pending_feedback(ctx.session_id) {
        emit_agent_event(&AgentEvent::FeedbackInjected {
            session_id: ctx.session_id.to_string(),
            kind: kind.clone(),
            content: content.clone(),
        })
        .await;
        append_message_to_contexts(
            &mut state.visible_messages,
            &mut state.recorded_messages,
            runtime_feedback_message(&kind, content),
        );
        call_messages = state.visible_messages.clone();
    }

    // Transient task-ledger injection; not persisted to history.
    let ledger_rendered = state.task_ledger.render_for_prompt();
    if !ledger_rendered.is_empty() {
        call_messages.push(serde_json::json!({
            "role": "user",
            "content": format!(
                "<runtime_injection kind=\"task_ledger\">\n{ledger_rendered}\n</runtime_injection>"
            ),
        }));
    }

    crate::llm::api::debug_log_message_shapes(
        &format!("agent iteration={} preflight", ctx.iteration),
        &call_messages,
    );

    opts.messages = call_messages;
    opts.system = call_system;
    Ok(())
}
