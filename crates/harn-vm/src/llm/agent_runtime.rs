//! Process-wide runtime helpers shared between the host and the
//! Harn-driven agent loop in `std/agent/loop.harn`.
//!
//! The legacy Rust agent loop has been retired (see #1197). What remains
//! here is the small surface that still has to live in Rust because it
//! either touches process-global state (event sinks, session-end hooks,
//! cross-thread feedback queues) or hands the host a thread-local
//! channel for the active session id and bridge.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Mutex};

use crate::agent_events::{self, AgentEvent, AgentEventSink};
use crate::mcp::VmMcpClientHandle;
use crate::value::VmValue;

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
    live: BTreeSet<(String, String)>,
}

impl ToolLifecycleStarts {
    fn observe(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::ToolCall {
                session_id,
                tool_call_id,
                ..
            } => {
                if tool_call_id.trim().is_empty() {
                    return true;
                }
                self.live.insert((session_id.clone(), tool_call_id.clone()))
            }
            AgentEvent::ToolCallUpdate {
                session_id,
                tool_call_id,
                status:
                    crate::agent_events::ToolCallStatus::Completed
                    | crate::agent_events::ToolCallStatus::Failed,
                ..
            } => {
                self.live
                    .remove(&(session_id.clone(), tool_call_id.clone()));
                true
            }
            _ => true,
        }
    }

    fn clear_session(&mut self, session_id: &str) {
        self.live
            .retain(|(active_session_id, _)| active_session_id != session_id);
    }
}

static TOOL_LIFECYCLE_STARTS: LazyLock<Mutex<ToolLifecycleStarts>> =
    LazyLock::new(|| Mutex::new(ToolLifecycleStarts::default()));

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
pub(crate) fn fire_session_end_hooks(session_id: &str) {
    TOOL_LIFECYCLE_STARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear_session(session_id);
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
    use crate::agent_events::{ToolCallStatus, ToolMutationStatus};
    use serde_json::json;

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
}
