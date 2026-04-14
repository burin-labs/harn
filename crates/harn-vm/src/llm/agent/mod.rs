use std::rc::Rc;

use crate::agent_events::{self, AgentEvent};
use crate::value::{VmError, VmValue};

// Imports from extracted submodules.
use super::agent_config::AgentLoopConfig;

mod finalize;
mod helpers;
mod llm_call;
mod post_turn;
mod state;
mod tool_dispatch;
mod turn_preflight;

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
/// **Thread-local invariant.** Pipeline closure subscribers are stored
/// in a `thread_local!` registry in `agent_events.rs` because
/// `VmValue` wraps `Rc` and can't cross threads. The agent loop itself
/// runs on a tokio `LocalSet`-pinned task, and `agent_subscribe`
/// (the host builtin that populates the registry) runs on that same
/// task, so the invariant holds. If a future VM embedder runs the
/// loop from a multi-thread runtime without a `LocalSet`, closure
/// subscribers will silently decouple from their emit site. The
/// `debug_assert!` below catches that invariant violation in debug
/// builds; release builds tolerate the divergence rather than panic
/// on a misconfigured embedding.
async fn emit_agent_event(event: &AgentEvent) {
    // External (Rust-side) sinks first — they're always sync.
    agent_events::emit_event(event);

    // Pipeline closure subscribers — invoke via the async VM API.
    let subscribers = agent_events::closure_subscribers_for(event.session_id());
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
        // Log but do not propagate subscriber errors — one misbehaving
        // subscriber (e.g. a pipeline grounding handler with a type
        // error) must not tear down the agent loop. Silent drops hid
        // pipeline bugs; logging surfaces them without escalating.
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
    // Build the long-lived loop state (drop guards, prelude computations,
    // daemon snapshot resume). The original inline prelude now lives on
    // `AgentLoopState::new` — behavior must be identical.
    let mut state = state::AgentLoopState::new(opts, config)?;

    // Rebuild the `tools` borrow the loop body reads. `AgentLoopState::new`
    // already mutated `opts.native_tools` and `opts.tool_choice` so these
    // views are stable for the rest of the run.
    let tools_owned = opts.tools.clone();
    let tools_val = tools_owned.as_ref();

    // Snapshot the config fields the iteration loop reads as locals so
    // we don't hold an immutable borrow on `state.config` across the
    // loop body (which would conflict with the `&mut state` phase
    // methods take). `config.turn_policy` is `Option<TurnPolicy>`;
    // clone it once here rather than `.as_ref()`-ing through a borrow.
    let llm_retries: usize = state.config.llm_retries;
    let llm_backoff_ms: u64 = state.config.llm_backoff_ms;
    let turn_policy = state.config.turn_policy.clone();
    let stop_after_successful_tools = state.config.stop_after_successful_tools.clone();
    let post_turn_callback = state.config.post_turn_callback.clone();

    // Copy/clone bindings for identifiers that collide with argument
    // names, module paths, or pattern bindings (so renaming `state.foo`
    // at every callsite would be brittle). `bridge` is an `Option<Rc>`,
    // cheap to clone; the rest are small scalars or already-cloned
    // owned values.
    let bridge = state.bridge.clone();
    let max_iterations: usize = state.max_iterations;
    let max_nudges: usize = state.max_nudges;
    let tool_retries: usize = state.tool_retries;
    let tool_backoff_ms: u64 = state.tool_backoff_ms;
    let exit_when_verified: bool = state.exit_when_verified;
    let persistent: bool = state.persistent;
    let daemon: bool = state.daemon;
    let has_tools: bool = state.has_tools;
    let loop_detect_enabled: bool = state.loop_detect_enabled;
    let resumed_iterations: usize = state.resumed_iterations;
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

    for iteration in 0..max_iterations {
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
                tools_val,
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
                        tools_val,
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
            post_turn::IterationOutcome::Break => break,
        }
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
