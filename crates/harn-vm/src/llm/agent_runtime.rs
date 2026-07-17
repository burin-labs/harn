//! Process-wide runtime helpers shared between the host and the
//! Harn-driven agent loop in `std/agent/loop.harn`.
//!
//! The legacy Rust agent loop has been retired (see #1197). What remains
//! here is the small surface that still has to live in Rust because it
//! either touches process-global state (event sinks, session-end hooks,
//! cross-thread feedback queues) or hands the host a thread-local
//! channel for the active session id and bridge.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::agent_events::{
    self, AgentEvent, AgentEventSink, ToolCallErrorCategory, ToolCallStatus, ToolMutationStatus,
};
use crate::mcp::VmMcpClientHandle;
use crate::value::VmValue;

/// Model-facing reason attached to the terminal update synthesized for a tool
/// call still in flight when its session finalizes (harn#4733).
const LOOP_EXIT_ABANDON_REASON: &str =
    "The agent loop exited while this tool call was still in flight; it was \
     never dispatched to a result. No terminal update was emitted by the \
     dispatch path, so the loop resolved it as abandoned at loop exit.";

/// Boxed session-end hook: receives a `session_id` string.
pub type SessionEndHook = Arc<dyn Fn(&str) + Send + Sync>;

thread_local! {
    static CURRENT_HOST_BRIDGE: RefCell<Option<Arc<crate::bridge::HostBridge>>> =
        const { RefCell::new(None) };
    /// Stack of per-loop event sinks installed via `LoopSinkGuard`. The
    /// agent loop pushes on entry and pops on drop; `emit_agent_event`
    /// fans events out to the top-of-stack sink in addition to the
    /// global `agent_events` registry. Distinct from the global registry
    /// on purpose: tests that wipe the global registry cannot race with
    /// a per-loop observation, and the host gets a non-cancellable
    /// observation path that's guaranteed to fire even when no external
    /// session subscriber is registered. Stack-shaped so nested loops
    /// (workflow stages, sub-agents) don't bleed events upward into the
    /// parent's sink.
    static CURRENT_LOOP_SINKS: RefCell<Vec<Arc<dyn AgentEventSink>>> =
        const { RefCell::new(Vec::new()) };
}

/// Registry of hooks called when an agent-loop session ends. Each hook
/// receives the `session_id` so it can release resources scoped to that
/// session (e.g. cancelling orphaned long-running handles).
static SESSION_END_HOOKS: LazyLock<Mutex<Vec<SessionEndHook>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

static SESSION_MCP_CLIENTS: LazyLock<Mutex<BTreeMap<String, BTreeMap<String, VmMcpClientHandle>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[derive(Default)]
struct ToolLifecycleStarts {
    /// `(session_id, tool_call_id)` → `tool_name` for every tool call that has
    /// emitted a `ToolCall` start but not yet a terminal
    /// `ToolCallUpdate { Completed | Failed }`. The `tool_name` is retained so a
    /// terminal update synthesized at session finalize (harn#4733) carries the
    /// same name as the original start.
    live: BTreeMap<(String, String), String>,
}

impl ToolLifecycleStarts {
    fn observe(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::ToolCall {
                session_id,
                tool_call_id,
                tool_name,
                ..
            } => {
                if tool_call_id.trim().is_empty() {
                    return true;
                }
                self.live
                    .insert(
                        (session_id.clone(), tool_call_id.clone()),
                        tool_name.clone(),
                    )
                    .is_none()
            }
            AgentEvent::ToolCallUpdate {
                session_id,
                tool_call_id,
                status: ToolCallStatus::Completed | ToolCallStatus::Failed,
                ..
            } => {
                self.live
                    .remove(&(session_id.clone(), tool_call_id.clone()));
                true
            }
            _ => true,
        }
    }

    /// Remove every still-in-flight call for `session_id`, returning
    /// `(tool_call_id, tool_name)` for each so the caller can synthesize a
    /// terminal update. Callers that only need to release the entries (e.g.
    /// tests) use [`Self::clear_session`].
    fn drain_session(&mut self, session_id: &str) -> Vec<(String, String)> {
        let live = std::mem::take(&mut self.live);
        let mut drained = Vec::new();
        for ((active_session_id, tool_call_id), tool_name) in live {
            if active_session_id == session_id {
                drained.push((tool_call_id, tool_name));
            } else {
                self.live
                    .insert((active_session_id, tool_call_id), tool_name);
            }
        }
        drained
    }

    /// Release a session's in-flight entries without synthesizing terminal
    /// updates. Used when a session *suspends* (it may resume, so its in-flight
    /// calls are not abandoned) and by tests.
    fn clear_session(&mut self, session_id: &str) {
        let _ = self.drain_session(session_id);
    }
}

static TOOL_LIFECYCLE_STARTS: LazyLock<Mutex<ToolLifecycleStarts>> =
    LazyLock::new(|| Mutex::new(ToolLifecycleStarts::default()));

/// Drop every session's in-flight tool calls and MCP clients.
///
/// Both maps are keyed by session id and released per session at session end,
/// which a run that dies before finalizing never reaches. They are also
/// process-global, so those entries outlive the run — and session ids are not
/// guaranteed unique across runs. A later session reusing an id would inherit
/// the dead run's in-flight calls and be handed a synthesized terminal
/// `ToolCallUpdate { Failed, AbandonedAtLoopExit }` for a tool call it never
/// made, or resolve an MCP client belonging to a connection that is gone.
///
/// Deliberately silent about terminal updates: a reset means no consumer is
/// listening for this session's events, so synthesizing them would report a
/// history to nobody. `clear_session` is the same choice for a suspend.
pub(crate) fn reset_session_state() {
    TOOL_LIFECYCLE_STARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live
        .clear();
    SESSION_MCP_CLIENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn observe_tool_lifecycle(event: &AgentEvent) -> bool {
    TOOL_LIFECYCLE_STARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .observe(event)
}

/// RAII guard that pushes a per-loop event sink onto the
/// `CURRENT_LOOP_SINKS` stack and pops it on drop.
pub(crate) struct LoopSinkGuard {
    pushed: bool,
}

impl LoopSinkGuard {
    pub(crate) fn install(sink: Option<Arc<dyn AgentEventSink>>) -> Self {
        if let Some(sink) = sink {
            CURRENT_LOOP_SINKS.with(|stack| stack.borrow_mut().push(sink));
            Self { pushed: true }
        } else {
            Self { pushed: false }
        }
    }
}

impl Drop for LoopSinkGuard {
    fn drop(&mut self) {
        if self.pushed {
            CURRENT_LOOP_SINKS.with(|stack| {
                let _ = stack.borrow_mut().pop();
            });
        }
    }
}

/// Synchronously emit an event to external sinks (the global registry)
/// and to the top-of-stack per-loop sink installed by `LoopSinkGuard`.
/// A streaming transport may announce a tool call before the dispatch path;
/// the shared lifecycle tracker keeps those observation sinks single-start.
/// Skips closure subscribers because they are async + VM-bound and
/// cannot be safely awaited from sites that may run outside the agent
/// loop's `LocalSet` task — currently the SSE transport (#693) which
/// fires `ToolCall(Pending)` / `ToolCallUpdate(Pending, raw_input)` per
/// streamed delta.
///
/// Closure subscribers still see the canonical lifecycle (`Pending →
/// InProgress → Completed/Failed`) emitted later by the dispatch path
/// via `emit_agent_event` — this sync path is for the streaming-args
/// observation surface only.
pub(crate) fn emit_agent_event_sync(event: &AgentEvent) {
    if observe_tool_lifecycle(event) {
        agent_events::emit_event(event);
        let loop_sink = CURRENT_LOOP_SINKS.with(|stack| stack.borrow().last().cloned());
        if let Some(sink) = loop_sink {
            sink.handle_event(event);
        }
    }
}

/// Run `future` with a thread-local live event sink installed.
///
/// Transport adapters use this for per-request observation surfaces that should
/// not depend on the process-global external sink registry. The normal global
/// registry still receives every event via [`emit_agent_event_sync`] /
/// [`emit_agent_event_with_ctx`]; this scoped sink is an additional,
/// dispatch-local path that cannot be cleared by sibling reset code.
pub async fn scope_agent_event_sink<F, T>(sink: Option<Arc<dyn AgentEventSink>>, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let _guard = LoopSinkGuard::install(sink);
    future.await
}

/// Emit an event through both external sinks (sync) and closure
/// subscribers (async, via the agent-loop's VM context).
/// Duplicate live tool-call starts are withheld from observation sinks because
/// the streaming path already published them. Closure subscribers still receive
/// this canonical dispatch event and therefore retain their existing ordering.
///
/// **Thread-local invariant.** Pipeline closure subscribers live on the
/// session's `SessionState.subscribers` in `crate::agent_sessions`,
/// which is a `thread_local!` owned by the agent loop. The loop runs on
/// a tokio `LocalSet`-pinned task, and `agent_subscribe` appends on that
/// same task, so subscriber ordering stays deterministic.
pub(crate) async fn emit_agent_event_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: &AgentEvent,
) {
    if observe_tool_lifecycle(event) {
        agent_events::emit_event(event);

        let loop_sink = CURRENT_LOOP_SINKS.with(|stack| stack.borrow().last().cloned());
        if let Some(sink) = loop_sink {
            sink.handle_event(event);
        }
    }

    let subscribers = crate::agent_sessions::subscribers_for(event.session_id());
    if subscribers.is_empty() {
        return;
    }
    let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    let arg = crate::stdlib::json_to_vm_value(&payload);
    for closure in subscribers {
        let VmValue::Closure(closure) = closure else {
            continue;
        };
        let Some(ctx) = ctx else {
            continue;
        };
        let mut vm = ctx.child_vm();
        // Log but don't propagate: one broken subscriber must not tear
        // down the agent loop.
        let result = vm.call_closure_pub(&closure, &[arg.clone()]).await;
        ctx.forward_output(&vm.take_output());
        if let Err(err) = result {
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

// Legacy `push_pending_feedback_global` / `drain_global_pending_feedback` /
// `wait_for_global_pending_feedback` shims were removed in the unified
// inbox cutover. Producers and consumers now use
// `crate::orchestration::agent_inbox::{push, drain, wait_sync,
// wait_async}` directly so each call site can carry a typed source
// label, observe sequence numbers, and use the clock-aware async wait.

/// Register a hook that fires when any agent-loop session ends. The
/// hook receives the session id and must be `Send + Sync` so it can be
/// stored across threads. Idempotent registration is the caller's
/// responsibility.
pub fn register_session_end_hook(hook: SessionEndHook) {
    if let Ok(mut hooks) = SESSION_END_HOOKS.lock() {
        hooks.push(hook);
    }
}

/// Fire every registered session-end hook with `session_id`. Called by
/// the host's session-finalize primitive once a session has been removed
/// from the active session map.
///
/// `abandon_in_flight` is `true` when the session reached a genuine terminal
/// condition (completion judge `done`, max iterations, budget exhausted,
/// error, stuck) and `false` when it merely *suspended* — a suspended session
/// may resume, so its in-flight calls are released without a terminal update
/// rather than falsely resolved as abandoned.
///
/// On a terminal exit, before firing the hooks, resolve any tool call still in
/// flight for this session: the loop reached a terminal condition while the
/// call was streamed but never dispatched to a result, so no `Completed`/
/// `Failed` update was ever emitted. For each such call this synthesizes one
/// terminal `ToolCallUpdate` (status `failed`, category `abandoned_at_loop_exit`)
/// so the transcript never ends with a dangling `pending` call (harn#4733). The
/// updates are emitted through [`emit_agent_event_sync`] — the same path the
/// streaming transport used to publish the original `ToolCall` start — so they
/// land in exactly the sinks that recorded the start.
pub(crate) fn fire_session_end_hooks(session_id: &str, abandon_in_flight: bool) {
    // Drain (not just clear) while holding the lock, then release it before
    // emitting: `emit_agent_event_sync` re-enters the lifecycle tracker to
    // observe each update, and the tracker mutex is not reentrant.
    let abandoned = {
        let mut tracker = TOOL_LIFECYCLE_STARTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if abandon_in_flight {
            tracker.drain_session(session_id)
        } else {
            // Suspended: release tracking, but do not synthesize terminal
            // updates — the calls may resume.
            tracker.clear_session(session_id);
            Vec::new()
        }
    };
    for (tool_call_id, tool_name) in abandoned {
        emit_agent_event_sync(&AgentEvent::ToolCallUpdate {
            session_id: session_id.to_string(),
            tool_call_id,
            tool_name,
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some(LOOP_EXIT_ABANDON_REASON.to_string()),
            duration_ms: None,
            execution_duration_ms: None,
            error_category: Some(ToolCallErrorCategory::AbandonedAtLoopExit),
            mutation_status: ToolMutationStatus::Unknown,
            changed_paths: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
    }
    if let Ok(hooks) = SESSION_END_HOOKS.lock() {
        for hook in hooks.iter() {
            hook(session_id);
        }
    }
}

pub(crate) fn install_current_host_bridge(bridge: Arc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn clear_current_host_bridge() {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub(crate) fn swap_current_host_bridge(
    bridge: Option<Arc<crate::bridge::HostBridge>>,
) -> Option<Arc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), bridge))
}

pub(crate) fn current_host_bridge() -> Option<Arc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

/// Return the active agent session id, if any. The session stack lives
/// in `crate::agent_sessions` and is pushed by
/// `host_agent_session_init` / popped by `host_agent_session_finalize`.
pub fn current_agent_session_id() -> Option<String> {
    crate::agent_sessions::current_session_id()
}

/// Install (or merge in) MCP client handles for a session. Merges by server
/// name so an incremental `__host_mcp_bootstrap` — used by mid-conversation
/// skill-declared MCP mounting — adds new servers without dropping the live
/// handles of servers mounted by an earlier bootstrap. A same-named entry is
/// overwritten with the freshly connected handle. On the initial bootstrap
/// the session has no entry, so this is identical to a plain insert.
pub(crate) fn install_session_mcp_clients(
    session_id: &str,
    clients: BTreeMap<String, VmMcpClientHandle>,
) {
    if let Ok(mut map) = SESSION_MCP_CLIENTS.lock() {
        let existing = map.entry(session_id.to_string()).or_default();
        for (name, handle) in clients {
            existing.insert(name, handle);
        }
    }
}

pub(crate) fn take_session_mcp_clients(
    session_id: &str,
) -> Option<BTreeMap<String, VmMcpClientHandle>> {
    SESSION_MCP_CLIENTS
        .lock()
        .ok()
        .and_then(|mut map| map.remove(session_id))
}

pub(crate) fn session_mcp_client(session_id: &str, server_name: &str) -> Option<VmMcpClientHandle> {
    SESSION_MCP_CLIENTS.lock().ok().and_then(|map| {
        map.get(session_id)
            .and_then(|clients| clients.get(server_name))
            .cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{ToolCallErrorCategory, ToolCallStatus, ToolMutationStatus};
    use serde_json::json;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEvent>>,
    }

    impl AgentEventSink for RecordingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.events
                .lock()
                .expect("recording sink")
                .push(event.clone());
        }
    }

    fn start(session_id: &str, tool_call_id: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "verify".to_string(),
            kind: None,
            status: ToolCallStatus::Pending,
            raw_input: json!({}),
            parsing: None,
            audit: None,
        }
    }

    fn finish(session_id: &str, tool_call_id: &str) -> AgentEvent {
        AgentEvent::ToolCallUpdate {
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "verify".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: ToolMutationStatus::Unknown,
            changed_paths: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        }
    }

    #[test]
    fn lifecycle_start_is_single_writer_until_terminal_update() {
        let mut starts = ToolLifecycleStarts::default();

        assert!(starts.observe(&start("session-a", "tool-1")));
        assert!(!starts.observe(&start("session-a", "tool-1")));
        assert!(starts.observe(&start("session-a", "tool-2")));
        assert!(starts.observe(&start("session-b", "tool-1")));
        assert!(starts.observe(&finish("session-a", "tool-1")));
        assert!(starts.observe(&start("session-a", "tool-1")));
    }

    #[test]
    fn lifecycle_start_session_cleanup_releases_unfinished_ids() {
        let mut starts = ToolLifecycleStarts::default();
        assert!(starts.observe(&start("session-a", "tool-1")));

        starts.clear_session("session-a");

        assert!(starts.observe(&start("session-a", "tool-1")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observation_sink_receives_one_start_across_stream_and_dispatch() {
        const SESSION_ID: &str = "single-start-observation-test";
        const TOOL_CALL_ID: &str = "tool-1";
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            starts.clear_session(SESSION_ID);
        }
        let sink = Arc::new(RecordingSink::default());
        let _guard = LoopSinkGuard::install(Some(sink.clone()));

        emit_agent_event_sync(&start(SESSION_ID, TOOL_CALL_ID));
        emit_agent_event_with_ctx(None, &start(SESSION_ID, TOOL_CALL_ID)).await;
        emit_agent_event_with_ctx(None, &finish(SESSION_ID, TOOL_CALL_ID)).await;

        let events = sink.events.lock().expect("recorded events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ToolCall { .. }))
                .count(),
            1,
            "the streaming and dispatch paths share one observable start authority"
        );
        assert_eq!(events.len(), 2, "one start and one terminal update remain");
    }

    /// Falsifier for harn#4733: a session that finalizes with a tool call still
    /// in flight (a `ToolCall` start, no terminal update) must have that call
    /// resolved with exactly one terminal `ToolCallUpdate` — never left dangling
    /// as `pending`. The sweep is per-session and idempotent.
    #[tokio::test(flavor = "current_thread")]
    async fn loop_exit_resolves_every_in_flight_call_to_a_terminal_update() {
        const SESSION_ID: &str = "loop-exit-abandon-test";
        const OTHER_SESSION: &str = "loop-exit-abandon-other";
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            starts.clear_session(SESSION_ID);
            starts.clear_session(OTHER_SESSION);
        }
        let sink = Arc::new(RecordingSink::default());
        let _guard = LoopSinkGuard::install(Some(sink.clone()));

        // Two calls go in flight for this session; a third belongs to a
        // *different* session and must survive this session's finalize.
        emit_agent_event_sync(&start(SESSION_ID, "call-a"));
        emit_agent_event_sync(&start(SESSION_ID, "call-b"));
        emit_agent_event_sync(&start(OTHER_SESSION, "call-c"));

        fire_session_end_hooks(SESSION_ID, true);

        let abandoned: Vec<(String, String, String, Option<String>)> = {
            let events = sink.events.lock().expect("recorded events");
            events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::ToolCallUpdate {
                        session_id,
                        tool_call_id,
                        tool_name,
                        status: ToolCallStatus::Failed,
                        error,
                        error_category: Some(ToolCallErrorCategory::AbandonedAtLoopExit),
                        ..
                    } => Some((
                        session_id.clone(),
                        tool_call_id.clone(),
                        tool_name.clone(),
                        error.clone(),
                    )),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(
            abandoned.len(),
            2,
            "both in-flight calls for the finalizing session get one terminal update each"
        );
        for (session_id, _id, tool_name, error) in &abandoned {
            assert_eq!(session_id, SESSION_ID);
            assert_eq!(
                tool_name, "verify",
                "the terminal update carries the tool name from the observed start"
            );
            assert!(
                error.as_deref().is_some_and(|reason| !reason.is_empty()),
                "the abandoned call carries a human-readable reason"
            );
        }
        let resolved: BTreeSet<&str> = abandoned.iter().map(|(_, id, _, _)| id.as_str()).collect();
        assert_eq!(
            resolved,
            BTreeSet::from(["call-a", "call-b"]),
            "a different session's in-flight call is not swept"
        );

        // Idempotent: re-finalizing the now-drained session emits nothing more.
        let before = sink.events.lock().expect("recorded events").len();
        fire_session_end_hooks(SESSION_ID, true);
        let after = sink.events.lock().expect("recorded events").len();
        assert_eq!(before, after, "re-finalizing a drained session is a no-op");

        // Hygiene: release the surviving other-session entry from the global tracker.
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            starts.clear_session(OTHER_SESSION);
        }
    }

    /// A call that already reached a terminal `Completed`/`Failed` update is
    /// removed from the in-flight set, so loop exit must not resurrect it as an
    /// abandoned call (harn#4733).
    #[tokio::test(flavor = "current_thread")]
    async fn loop_exit_does_not_resurrect_a_completed_call() {
        const SESSION_ID: &str = "loop-exit-completed-test";
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            starts.clear_session(SESSION_ID);
        }
        let sink = Arc::new(RecordingSink::default());
        let _guard = LoopSinkGuard::install(Some(sink.clone()));

        emit_agent_event_sync(&start(SESSION_ID, "call-done"));
        emit_agent_event_sync(&finish(SESSION_ID, "call-done"));
        fire_session_end_hooks(SESSION_ID, true);

        let events = sink.events.lock().expect("recorded events");
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AgentEvent::ToolCallUpdate {
                    error_category: Some(ToolCallErrorCategory::AbandonedAtLoopExit),
                    ..
                }
            )),
            "a call that already reached a terminal update is not swept at loop exit"
        );
    }

    /// A run that dies before finalizing never fires its session-end hooks, and
    /// this tracker is process-global while session ids are not unique across
    /// runs. Without a reset drain, the NEXT run to use the id inherits the dead
    /// run's in-flight calls and is handed an abandoned terminal update for a
    /// tool call it never made — a fabricated event, which is worse than a
    /// missing one.
    #[tokio::test(flavor = "current_thread")]
    async fn reset_drops_in_flight_calls_so_a_later_session_cannot_inherit_them() {
        const SESSION_ID: &str = "loop-exit-reset-test";
        emit_agent_event_sync(&start(SESSION_ID, "call-from-a-dead-run"));

        crate::reset_thread_local_state();

        let sink = Arc::new(RecordingSink::default());
        let _guard = LoopSinkGuard::install(Some(sink.clone()));
        // The later run ends the same session id, abandoning in-flight calls.
        fire_session_end_hooks(SESSION_ID, true);

        let events = sink.events.lock().expect("recorded events");
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AgentEvent::ToolCallUpdate {
                    error_category: Some(ToolCallErrorCategory::AbandonedAtLoopExit),
                    ..
                }
            )),
            "a reset run's in-flight calls must not surface as the next run's abandoned calls"
        );
    }

    /// A *suspended* session may resume, so finalize must release its in-flight
    /// calls without synthesizing an abandoned terminal update — otherwise a
    /// call that resumes would carry a false `failed` result (harn#4733).
    #[tokio::test(flavor = "current_thread")]
    async fn suspend_releases_in_flight_calls_without_a_terminal_update() {
        const SESSION_ID: &str = "loop-exit-suspend-test";
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            starts.clear_session(SESSION_ID);
        }
        let sink = Arc::new(RecordingSink::default());
        let _guard = LoopSinkGuard::install(Some(sink.clone()));

        emit_agent_event_sync(&start(SESSION_ID, "call-suspended"));
        fire_session_end_hooks(SESSION_ID, false);

        let events = sink.events.lock().expect("recorded events");
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AgentEvent::ToolCallUpdate {
                    error_category: Some(ToolCallErrorCategory::AbandonedAtLoopExit),
                    ..
                }
            )),
            "a suspended session must not emit an abandoned terminal update"
        );
        // Tracking was still released, so re-observing the same start is fresh.
        drop(events);
        if let Ok(mut starts) = TOOL_LIFECYCLE_STARTS.lock() {
            assert!(
                starts.observe(&start(SESSION_ID, "call-suspended")),
                "suspend released the in-flight entry"
            );
            starts.clear_session(SESSION_ID);
        }
    }
}
