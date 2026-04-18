use std::rc::Rc;

use crate::agent_events::{self, AgentEvent};
use crate::value::{VmError, VmValue};

use super::agent_config::AgentLoopConfig;

mod finalize;
mod helpers;
mod llm_call;
mod post_turn;
mod skill_match;
mod state;
mod tool_dispatch;
mod tool_search_client;
mod turn_preflight;

pub use skill_match::parse_skill_config;
pub use state::SkillMatchConfig;
#[allow(unused_imports)]
pub use state::{ActiveSkill, SkillMatchStrategy};

thread_local! {
    static CURRENT_HOST_BRIDGE: std::cell::RefCell<Option<Rc<crate::bridge::HostBridge>>> = const { std::cell::RefCell::new(None) };
    /// Queue of feedback items pushed via `agent_inject_feedback(session_id, kind, content)`
    /// from inside a pipeline event handler. The turn loop drains this
    /// queue at safe boundaries (before each LLM call) and appends each
    /// entry as a runtime-feedback message.
    static PENDING_FEEDBACK: std::cell::RefCell<Vec<(String, String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Emit an event through both external sinks (sync) and closure
/// subscribers (async, via the agent-loop's VM context). Called by the
/// turn loop at every phase.
///
/// **Thread-local invariant.** Pipeline closure subscribers live on the
/// session's `SessionState.subscribers` in `crate::agent_sessions`,
/// which is a `thread_local!` because `VmValue` wraps `Rc` and can't
/// cross threads. The agent loop runs on a tokio `LocalSet`-pinned
/// task, and `agent_subscribe` (the host builtin that appends to the
/// session) runs on that same task, so the invariant holds. If a
/// future VM embedder runs the loop from a multi-thread runtime
/// without a `LocalSet`, closure subscribers will silently decouple
/// from their emit site.
async fn emit_agent_event(event: &AgentEvent) {
    agent_events::emit_event(event);

    let subscribers = crate::agent_sessions::subscribers_for(event.session_id());
    if subscribers.is_empty() {
        return;
    }
    let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    for closure in subscribers {
        let VmValue::Closure(closure) = closure else {
            continue;
        };
        let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
            continue;
        };
        let arg = crate::stdlib::json_to_vm_value(&payload);
        // Log but don't propagate: one broken subscriber must not tear
        // down the agent loop.
        if let Err(err) = vm.call_closure_pub(&closure, &[arg], &[]).await {
            crate::events::log_warn(
                "agent.subscriber",
                &format!(
                    "session={} event={:?} subscriber error: {}",
                    event.session_id(),
                    std::mem::discriminant(event),
                    err
                ),
            );
        }
    }
}

/// Push a pending-feedback item. Called by the `agent_inject_feedback`
/// host builtin; drained by the turn loop.
pub(crate) fn push_pending_feedback(session_id: &str, kind: &str, content: &str) {
    PENDING_FEEDBACK.with(|q| {
        q.borrow_mut().push((
            session_id.to_string(),
            kind.to_string(),
            content.to_string(),
        ))
    });
}

/// Drain every pending-feedback item for a session. Called by the turn
/// loop at injection boundaries.
pub(super) fn drain_pending_feedback(session_id: &str) -> Vec<(String, String)> {
    PENDING_FEEDBACK.with(|q| {
        let mut queue = q.borrow_mut();
        let mut drained: Vec<(String, String)> = Vec::new();
        let mut kept: Vec<(String, String, String)> = Vec::new();
        for (sid, kind, content) in queue.drain(..) {
            if sid == session_id {
                drained.push((kind, content));
            } else {
                kept.push((sid, kind, content));
            }
        }
        *queue = kept;
        drained
    })
}

/// RAII guard that binds the agent loop's tool registry as the thread's
/// current registry (for `tool_ref` / `tool_def` lookups) and restores
/// the previous binding on drop.
struct ToolRegistryGuard {
    previous: Option<VmValue>,
}

impl ToolRegistryGuard {
    fn install(registry: Option<VmValue>) -> Self {
        let previous = crate::stdlib::tools::install_current_tool_registry(registry);
        Self { previous }
    }
}

impl Drop for ToolRegistryGuard {
    fn drop(&mut self) {
        crate::stdlib::tools::install_current_tool_registry(self.previous.take());
    }
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

pub async fn run_agent_loop_internal(
    opts: &mut super::api::LlmCallOptions,
    config: AgentLoopConfig,
) -> Result<serde_json::Value, VmError> {
    let mut state = state::AgentLoopState::new(opts, config)?;

    let tools_owned = opts.tools.clone();
    let tools_val = tools_owned.as_ref();

    let _tool_registry_guard = ToolRegistryGuard::install(tools_owned.clone());

    // Snapshot config/state fields as locals so phase contexts can hold
    // them without fighting the `&mut state` borrow in the loop body.
    let llm_retries = state.config.llm_retries;
    let llm_backoff_ms = state.config.llm_backoff_ms;
    let turn_policy = state.config.turn_policy.clone();
    let stop_after_successful_tools = state.config.stop_after_successful_tools.clone();
    let post_turn_callback = state.config.post_turn_callback.clone();
    let bridge = state.bridge.clone();
    let max_iterations = state.max_iterations;
    let max_nudges = state.max_nudges;
    let tool_retries = state.tool_retries;
    let tool_backoff_ms = state.tool_backoff_ms;
    let exit_when_verified = state.exit_when_verified;
    let persistent = state.persistent;
    let daemon = state.daemon;
    let has_tools = state.has_tools;
    let loop_detect_enabled = state.loop_detect_enabled;
    let resumed_iterations = state.resumed_iterations;
    let tool_format = state.tool_format.clone();
    let done_sentinel = state.done_sentinel.clone();
    let break_unless_phase = state.break_unless_phase.clone();
    let loop_start = state.loop_start;
    let tool_contract_prompt = state.tool_contract_prompt.clone();
    let base_system = state.base_system.clone();
    let persistent_system_prompt = state.persistent_system_prompt.clone();
    let auto_compact = state.auto_compact.clone();
    let daemon_config = state.daemon_config.clone();
    let custom_nudge = state.custom_nudge.clone();
    let session_id = state.session_id.clone();

    // Warn on unknown `stop_after_successful_tools` names: they're
    // tolerated (forward-compat with late-declared tools) but silently
    // never stopping is the failure mode to guard against.
    if let Some(stop_tools) = stop_after_successful_tools.as_ref() {
        let declared = super::tools::collect_tool_schemas(tools_val, opts.native_tools.as_deref());
        let declared_names: std::collections::BTreeSet<&str> =
            declared.iter().map(|schema| schema.name.as_str()).collect();
        let unknown: Vec<&str> = stop_tools
            .iter()
            .filter(|name| !declared_names.contains(name.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            crate::events::log_warn(
                "agent.stop_after_successful_tools",
                &format!(
                    "name(s) not in declared tool schema: {} — will never trigger a stop unless declared later",
                    unknown.join(", ")
                ),
            );
        }
    }

    let mut iteration_exited_via_break = false;
    for iteration in 0..max_iterations {
        // Skill matching runs at the head of iteration 0 (always) and,
        // when sticky=false, again before each subsequent iteration.
        // Reassess-in-place keeps the active skill when nothing
        // changed, so sticky=true + a still-applicable skill stays hot
        // for the rest of the loop.
        //
        // Exception: if this loop resumed a persisted session whose
        // previous run left skills active, iteration 0 preserves that
        // set instead of re-matching from a cold prompt. sticky=false
        // still lets the post-turn reassess run after turn 1.
        let skip_initial_match =
            iteration == 0 && state.rehydrated_from_session && state.skill_match.sticky;
        let should_match = (iteration == 0 || !state.skill_match.sticky) && !skip_initial_match;
        if should_match {
            skill_match::run_skill_match(
                &mut state,
                opts,
                &bridge,
                &session_id,
                iteration,
                iteration > 0,
            )
            .await?;
        }

        // If any active skill narrows the tool surface via
        // `allowed_tools`, compute the scoped registry for this turn.
        // Downstream phases see the narrowed view; the original
        // `tools_owned` stays intact so deactivation restores the full
        // surface on a later iteration.
        let scoped_tools = state.skill_scoped_tools_val(tools_val);
        let effective_tools_val: Option<&crate::value::VmValue> =
            scoped_tools.as_ref().or(tools_val);

        turn_preflight::run_turn_preflight(
            &mut state,
            opts,
            turn_preflight::PreflightContext {
                bridge: &bridge,
                session_id: &session_id,
                resumed_iterations,
                iteration,
                base_system: base_system.as_deref(),
                tool_contract_prompt: tool_contract_prompt.as_deref(),
                persistent_system_prompt: persistent_system_prompt.as_deref(),
                scoped_tools_val: scoped_tools.as_ref(),
            },
        )
        .await?;

        let mut call_result = llm_call::run_llm_call(
            &mut state,
            opts,
            &llm_call::LlmCallContext {
                bridge: &bridge,
                tool_format: &tool_format,
                done_sentinel: &done_sentinel,
                break_unless_phase: break_unless_phase.as_deref(),
                exit_when_verified,
                persistent,
                has_tools,
                turn_policy: turn_policy.as_ref(),
                llm_retries,
                llm_backoff_ms,
                tools_val: effective_tools_val,
            },
            iteration,
        )
        .await?;

        let dispatch = if !call_result.tool_calls.is_empty() {
            Some(
                tool_dispatch::run_tool_dispatch(
                    &mut state,
                    opts,
                    &tool_dispatch::ToolDispatchContext {
                        bridge: &bridge,
                        tool_format: &tool_format,
                        tools_val: effective_tools_val,
                        tool_retries,
                        tool_backoff_ms,
                        loop_detect_enabled,
                        session_id: &session_id,
                        iteration,
                        exit_when_verified,
                        auto_compact: &auto_compact,
                    },
                    &call_result,
                )
                .await?,
            )
        } else {
            None
        };

        match post_turn::run_post_turn(
            &mut state,
            opts,
            &post_turn::PostTurnContext {
                bridge: &bridge,
                session_id: &session_id,
                tool_format: &tool_format,
                max_nudges,
                persistent,
                daemon,
                turn_policy: turn_policy.as_ref(),
                stop_after_successful_tools: &stop_after_successful_tools,
                post_turn_callback: &post_turn_callback,
                auto_compact: &auto_compact,
                daemon_config: &daemon_config,
                custom_nudge: &custom_nudge,
                iteration,
            },
            &mut call_result,
            dispatch,
        )
        .await?
        {
            post_turn::IterationOutcome::Continue => continue,
            post_turn::IterationOutcome::Break => {
                iteration_exited_via_break = true;
                break;
            }
        }
    }

    // Hit the iteration budget rather than breaking — signal distinctly
    // so hosts can tell "done" from "ran out of rope".
    if !iteration_exited_via_break && max_iterations > 0 {
        state.final_status = "budget_exhausted";
        emit_agent_event(&AgentEvent::BudgetExhausted {
            session_id: session_id.clone(),
            max_iterations,
        })
        .await;
    }

    finalize::run_finalize(
        &mut state,
        opts,
        bridge,
        daemon,
        &daemon_config,
        &tool_format,
        loop_start,
    )
    .await
}

#[cfg(test)]
mod tests;
