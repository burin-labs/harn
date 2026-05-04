//! Process-wide runtime helpers shared between the host and the
//! Harn-driven agent loop in `std/agent/loop.harn`.
//!
//! The legacy Rust agent loop has been retired (see #1197). What remains
//! here is the small surface that still has to live in Rust because it
//! either touches process-global state (event sinks, session-end hooks,
//! cross-thread feedback queues) or hands the host a thread-local
//! channel for the active session id and bridge.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use crate::agent_events::{self, AgentEvent, AgentEventSink};
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

/// Global (cross-thread) pending feedback queue. Background threads
/// (e.g. long-running tool monitors) push here so the agent loop can
/// drain them at safe boundaries.
static GLOBAL_PENDING_FEEDBACK: LazyLock<Mutex<Vec<(String, String, String)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// Paired notifier so test helpers can wait for new entries without a
/// poll loop. `push_pending_feedback_global` notifies after pushing;
/// `wait_for_global_pending_feedback` parks on this condvar with a
/// timeout.
static GLOBAL_PENDING_FEEDBACK_CV: LazyLock<Condvar> = LazyLock::new(Condvar::new);

/// Registry of hooks called when an agent-loop session ends. Each hook
/// receives the `session_id` so it can release resources scoped to that
/// session (e.g. cancelling orphaned long-running handles).
static SESSION_END_HOOKS: LazyLock<Mutex<Vec<SessionEndHook>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

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
pub(crate) async fn emit_agent_event(event: &AgentEvent) {
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
        if let Err(err) = vm.call_closure_pub(&closure, &[arg]).await {
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

/// Push a pending-feedback item from any thread. Background tasks (e.g.
/// long-running tool monitors) use this to deliver results; the agent
/// loop drains it at injection boundaries.
pub fn push_pending_feedback_global(session_id: &str, kind: &str, content: &str) {
    if let Ok(mut q) = GLOBAL_PENDING_FEEDBACK.lock() {
        q.push((
            session_id.to_string(),
            kind.to_string(),
            content.to_string(),
        ));
    }
    GLOBAL_PENDING_FEEDBACK_CV.notify_all();
}

/// Test/integration helper: block (with timeout) until at least one
/// item with `session_id` is queued in the global pending-feedback
/// queue, then return without draining. Replaces `Instant::now()` +
/// `thread::sleep` polling loops in tests that observe background-thread
/// feedback. Returns `true` if an item with the matching session_id
/// appeared before the timeout, `false` on timeout. Spurious wake-ups
/// are absorbed by re-checking the queue inside the wait loop.
pub fn wait_for_global_pending_feedback(session_id: &str, timeout: Duration) -> bool {
    let Ok(mut guard) = GLOBAL_PENDING_FEEDBACK.lock() else {
        return false;
    };
    if guard.iter().any(|(sid, _, _)| sid == session_id) {
        return true;
    }
    let start = std::time::Instant::now();
    loop {
        let remaining = match timeout.checked_sub(start.elapsed()) {
            Some(remaining) if !remaining.is_zero() => remaining,
            _ => return guard.iter().any(|(sid, _, _)| sid == session_id),
        };
        let (next_guard, wait_result) =
            match GLOBAL_PENDING_FEEDBACK_CV.wait_timeout(guard, remaining) {
                Ok(pair) => pair,
                Err(poison) => {
                    let pair = poison.into_inner();
                    (pair.0, pair.1)
                }
            };
        guard = next_guard;
        if guard.iter().any(|(sid, _, _)| sid == session_id) {
            return true;
        }
        if wait_result.timed_out() {
            return false;
        }
    }
}

/// Drain every item for `session_id` from the global (cross-thread)
/// queue. Intended for integration tests that want to inspect feedback
/// pushed by background threads without running a full agent loop.
pub fn drain_global_pending_feedback(session_id: &str) -> Vec<(String, String)> {
    let mut drained = Vec::new();
    if let Ok(mut q) = GLOBAL_PENDING_FEEDBACK.lock() {
        let mut kept = Vec::new();
        for (sid, kind, content) in q.drain(..) {
            if sid == session_id {
                drained.push((kind, content));
            } else {
                kept.push((sid, kind, content));
            }
        }
        *q = kept;
    }
    drained
}

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
