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
    /// Skill-scoped tool registry for this turn, when skill activation
    /// narrowed `tools_val`. `None` when the full registry is in
    /// effect — the preflight then uses the baked-in `tool_contract_prompt`.
    pub scoped_tools_val: Option<&'a crate::value::VmValue>,
}

pub(super) async fn run_turn_preflight(
    state: &mut AgentLoopState,
    opts: &mut super::super::api::LlmCallOptions,
    ctx: PreflightContext<'_>,
) -> Result<(), VmError> {
    if let Some(bridge) = ctx.bridge.as_ref() {
        bridge.set_daemon_idle(false);
    }
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

    // Client-mode tool_search: regenerate the tool-contract prompt on
    // every turn so freshly-promoted deferred tools appear in the
    // schema list. Without this, turn 1's prompt (minus the deferred
    // tools) would be reused for turn N, and the model wouldn't see
    // the schemas the search tool just surfaced.
    let dynamic_contract_prompt = state.rebuild_tool_contract_prompt(opts);
    // Skill-scoped tool prompt: when an active skill narrows the tool
    // surface, rebuild the contract prompt against the scoped view so
    // the model's schema list matches the dispatch allowlist. Takes
    // priority over the baked-in snapshot but not over the dynamic
    // tool_search prompt.
    let scoped_contract_prompt = ctx
        .scoped_tools_val
        .filter(|_| dynamic_contract_prompt.is_none() && state.has_tools)
        .map(|tv| {
            crate::llm::tools::build_tool_calling_contract_prompt(
                Some(tv),
                opts.native_tools.as_deref(),
                &state.tool_format,
                state
                    .config
                    .turn_policy
                    .as_ref()
                    .is_some_and(|policy| policy.require_action_or_yield),
                state.config.tool_examples.as_deref(),
                !state.config.task_ledger.is_empty(),
            )
        });
    let tool_prompt_slot = dynamic_contract_prompt
        .as_deref()
        .or(scoped_contract_prompt.as_deref())
        .or(ctx.tool_contract_prompt);

    let prompt_skills = state.prompt_active_skills();
    let skill_prompt = render_active_skill_prompt(&prompt_skills);
    let base_with_skill = merge_optional(ctx.base_system, skill_prompt.as_deref());
    let default_system = build_agent_system_prompt(
        base_with_skill.as_deref(),
        tool_prompt_slot,
        ctx.persistent_system_prompt,
    );
    let mut call_messages = state.visible_messages.clone();
    let call_system = default_system;

    crate::orchestration::run_lifecycle_hooks(
        crate::orchestration::HookEvent::PreAgentTurn,
        &serde_json::json!({
            "event": crate::orchestration::HookEvent::PreAgentTurn.as_str(),
            "session": {
                "id": ctx.session_id,
            },
            "turn": {
                "iteration": ctx.iteration,
                "total_iterations": state.total_iterations,
            },
        }),
    )
    .await?;

    // Emit TurnStart before draining pending feedback so subscribers
    // see the boundary before any drain-generated injections land.
    emit_agent_event(&AgentEvent::TurnStart {
        session_id: ctx.session_id.to_string(),
        iteration: ctx.iteration,
    })
    .await;

    // Prefill injections are pulled off the pending-feedback queue and
    // assigned to `opts.prefill` instead of appended as a user-role
    // runtime-feedback message. The llm-call phase consumes and clears
    // `opts.prefill` each turn so injections apply once per turn.
    opts.prefill = None;
    for (kind, content) in drain_pending_feedback(ctx.session_id) {
        emit_agent_event(&AgentEvent::FeedbackInjected {
            session_id: ctx.session_id.to_string(),
            kind: kind.clone(),
            content: content.clone(),
        })
        .await;
        crate::llm::agent_observe::append_llm_observability_entry(
            "feedback_injected",
            serde_json::Map::from_iter([
                ("iteration".to_string(), serde_json::json!(ctx.iteration)),
                ("session_id".to_string(), serde_json::json!(ctx.session_id)),
                ("kind".to_string(), serde_json::json!(kind.clone())),
                ("content".to_string(), serde_json::json!(content.clone())),
            ]),
        );
        if kind == "prefill_assistant" {
            opts.prefill = Some(content);
            continue;
        }
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

    opts.messages = call_messages;
    opts.system = call_system;
    let _ =
        crate::llm::structural_experiments::apply_structural_experiment(opts, Some(ctx.iteration))
            .await?;
    call_messages = opts.messages.clone();

    crate::llm::api::debug_log_message_shapes(
        &format!("agent iteration={} preflight", ctx.iteration),
        &call_messages,
    );

    // Rebuild opts.native_tools from the baseline snapshot and apply
    // the active skill's allowed_tools whitelist. Idempotent across
    // turns — activation narrows, deactivation restores. Works whether
    // or not a scoped view currently applies (the helper handles both
    // paths).
    state.rebuild_scoped_native_tools(opts);

    opts.messages = call_messages;
    Ok(())
}

/// Render the active-skill block that gets appended to the base system
/// prompt. Each skill contributes its description (always) and body
/// prompt (when non-empty) under a `## Active skill: <name>` heading.
/// Returns `None` when no skills are active — callers fall back to the
/// unmodified base prompt.
fn render_active_skill_prompt(active: &[super::state::ActiveSkill]) -> Option<String> {
    if active.is_empty() {
        return None;
    }
    let mut out = String::from("\n\n## Active skills\n");
    for skill in active {
        out.push_str(&format!("\n### {}\n", skill.name));
        if !skill.description.is_empty() {
            out.push_str(&format!("{}\n", skill.description.trim()));
        }
        if !skill.when_to_use.is_empty() {
            out.push_str(&format!("When to use: {}\n", skill.when_to_use.trim()));
        }
        if !skill.allowed_tools.is_empty() {
            out.push_str(&format!(
                "Scoped tools: {}\n",
                skill.allowed_tools.join(", ")
            ));
        }
        if let Some(prompt) = skill.prompt.as_deref() {
            let trimmed = prompt.trim();
            if !trimmed.is_empty() {
                out.push('\n');
                out.push_str(trimmed);
                out.push('\n');
            }
        }
    }
    Some(out)
}

fn merge_optional(base: Option<&str>, extra: Option<&str>) -> Option<String> {
    match (base, extra) {
        (Some(b), Some(e)) => {
            let trimmed_b = b.trim_end();
            Some(format!("{trimmed_b}{e}"))
        }
        (Some(b), None) => Some(b.to_string()),
        (None, Some(e)) => Some(e.trim_start().to_string()),
        (None, None) => None,
    }
}
