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
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};

use crate::agent_events::{self, AgentEvent, AgentEventSink};
use crate::mcp::VmMcpClientHandle;
use crate::value::VmValue;

/// Boxed session-end hook: receives a `session_id` string.
pub type SessionEndHook = Arc<dyn Fn(&str) + Send + Sync>;

thread_local! {
    static CURRENT_HOST_BRIDGE: RefCell<Option<Rc<crate::bridge::HostBridge>>> =
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
    agent_events::emit_event(event);
    let loop_sink = CURRENT_LOOP_SINKS.with(|stack| stack.borrow().last().cloned());
    if let Some(sink) = loop_sink {
        sink.handle_event(event);
    }
}

/// Emit an event through both external sinks (sync) and closure
/// subscribers (async, via the agent-loop's VM context).
///
/// **Thread-local invariant.** Pipeline closure subscribers live on the
/// session's `SessionState.subscribers` in `crate::agent_sessions`,
/// which is a `thread_local!` because `VmValue` wraps `Rc` and can't
/// cross threads. The agent loop runs on a tokio `LocalSet`-pinned
/// task, and `agent_subscribe` (the host builtin that appends to the
/// session) runs on that same task, so the invariant holds.
pub(crate) async fn emit_agent_event_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: &AgentEvent,
) {
    agent_events::emit_event(event);

    let loop_sink = CURRENT_LOOP_SINKS.with(|stack| stack.borrow().last().cloned());
    if let Some(sink) = loop_sink {
        sink.handle_event(event);
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
    if let Ok(hooks) = SESSION_END_HOOKS.lock() {
        for hook in hooks.iter() {
            hook(session_id);
        }
    }
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn clear_current_host_bridge() {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

/// Return the active agent session id, if any. The session stack lives
/// in `crate::agent_sessions` and is pushed by
/// `host_agent_session_init` / popped by `host_agent_session_finalize`.
pub fn current_agent_session_id() -> Option<String> {
    crate::agent_sessions::current_session_id()
}

pub(crate) fn install_session_mcp_clients(
    session_id: &str,
    clients: BTreeMap<String, VmMcpClientHandle>,
) {
    if let Ok(mut map) = SESSION_MCP_CLIENTS.lock() {
        map.insert(session_id.to_string(), clients);
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
