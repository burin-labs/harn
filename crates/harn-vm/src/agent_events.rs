//! Agent event stream — the ACP-aligned observation surface for the
//! agent loop.
//!
//! Every phase of the turn loop emits an `AgentEvent`. The canonical
//! variants map 1:1 onto ACP `SessionUpdate` values; three internal
//! variants (`TurnStart`, `TurnEnd`, `FeedbackInjected`) let pipelines
//! react to loop milestones that don't have a direct ACP counterpart.
//!
//! There are two subscription paths, both keyed on session id so two
//! concurrent sessions never cross-talk:
//!
//! 1. **External sinks** (`AgentEventSink` trait) — Rust-side consumers
//!    like the harn-cli ACP server. Invoked synchronously by the loop.
//! 2. **Closure subscribers** — `.harn` closures registered via the
//!    `agent_subscribe(session_id, callback)` host builtin. Stored as
//!    raw `VmValue`s and invoked by the agent loop in its async VM
//!    context (the sink trait can't easily await, so this path
//!    intentionally bypasses it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::tool_annotations::ToolKind;
use crate::value::VmValue;

/// Status of a tool call. Mirrors ACP's `toolCallStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Dispatched by the model but not yet started.
    Pending,
    /// Dispatch is actively running.
    InProgress,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

/// Events emitted by the agent loop. The first five variants map 1:1
/// to ACP `sessionUpdate` variants; the last three are harn-internal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentMessageChunk {
        session_id: String,
        content: String,
    },
    AgentThoughtChunk {
        session_id: String,
        content: String,
    },
    ToolCall {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        kind: Option<ToolKind>,
        status: ToolCallStatus,
        raw_input: serde_json::Value,
    },
    ToolCallUpdate {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        status: ToolCallStatus,
        raw_output: Option<serde_json::Value>,
        error: Option<String>,
    },
    Plan {
        session_id: String,
        plan: serde_json::Value,
    },
    // ── Internal (no direct ACP equivalent) ────────────────────────
    TurnStart {
        session_id: String,
        iteration: usize,
    },
    TurnEnd {
        session_id: String,
        iteration: usize,
        turn_info: serde_json::Value,
    },
    FeedbackInjected {
        session_id: String,
        kind: String,
        content: String,
    },
    /// Emitted when the agent loop exhausts `max_iterations` without any
    /// explicit break condition firing. Distinct from a natural "done" or
    /// a "stuck" nudge-exhaustion: this is strictly a budget cap.
    BudgetExhausted {
        session_id: String,
        max_iterations: usize,
    },
    /// Emitted when the loop breaks because consecutive text-only turns
    /// hit `max_nudges`. Parity with `BudgetExhausted` / `TurnEnd` for
    /// hosts that key off agent-terminal events.
    LoopStuck {
        session_id: String,
        max_nudges: usize,
        last_iteration: usize,
        tail_excerpt: String,
    },
    /// Emitted when the daemon idle-wait loop trips its watchdog because
    /// every configured wake source returned `None` for N consecutive
    /// attempts. Exists so a broken daemon doesn't hang the session
    /// silently.
    DaemonWatchdogTripped {
        session_id: String,
        attempts: usize,
        elapsed_ms: u64,
    },
}

impl AgentEvent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::AgentMessageChunk { session_id, .. }
            | Self::AgentThoughtChunk { session_id, .. }
            | Self::ToolCall { session_id, .. }
            | Self::ToolCallUpdate { session_id, .. }
            | Self::Plan { session_id, .. }
            | Self::TurnStart { session_id, .. }
            | Self::TurnEnd { session_id, .. }
            | Self::FeedbackInjected { session_id, .. }
            | Self::BudgetExhausted { session_id, .. }
            | Self::LoopStuck { session_id, .. }
            | Self::DaemonWatchdogTripped { session_id, .. } => session_id,
        }
    }
}

/// External consumers of the event stream (e.g. the harn-cli ACP server,
/// which translates events into JSON-RPC notifications).
pub trait AgentEventSink: Send + Sync {
    fn handle_event(&self, event: &AgentEvent);
}

/// Fan-out helper for composing multiple external sinks.
pub struct MultiSink {
    sinks: Mutex<Vec<Arc<dyn AgentEventSink>>>,
}

impl MultiSink {
    pub fn new() -> Self {
        Self {
            sinks: Mutex::new(Vec::new()),
        }
    }
    pub fn push(&self, sink: Arc<dyn AgentEventSink>) {
        self.sinks.lock().expect("sink mutex poisoned").push(sink);
    }
    pub fn len(&self) -> usize {
        self.sinks.lock().expect("sink mutex poisoned").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MultiSink {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventSink for MultiSink {
    fn handle_event(&self, event: &AgentEvent) {
        // Deliberate: snapshot then release the lock before invoking sink
        // callbacks. Sinks can re-enter the event system (e.g. a host
        // sink that logs to another AgentEvent path), so holding the
        // mutex across the callback would risk self-deadlock. Arc clones
        // are refcount bumps — cheap.
        let sinks = self.sinks.lock().expect("sink mutex poisoned").clone();
        for sink in sinks {
            sink.handle_event(event);
        }
    }
}

// ── Registries ──────────────────────────────────────────────────────

type ExternalSinkRegistry = RwLock<HashMap<String, Vec<Arc<dyn AgentEventSink>>>>;

fn external_sinks() -> &'static ExternalSinkRegistry {
    static REGISTRY: OnceLock<ExternalSinkRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

// Closure subscribers are stored thread-local because `VmValue`
// contains `Rc`, which is neither `Send` nor `Sync`. The agent loop
// runs on a single tokio current-thread worker, so all subscribe /
// emit calls happen on the same thread — thread-local storage is
// correct here.
thread_local! {
    static CLOSURE_SUBSCRIBERS: RefCell<HashMap<String, Vec<VmValue>>> =
        RefCell::new(HashMap::new());
}

// Register an external (Rust-side) sink for a session.
pub fn register_sink(session_id: impl Into<String>, sink: Arc<dyn AgentEventSink>) {
    let session_id = session_id.into();
    let mut reg = external_sinks().write().expect("sink registry poisoned");
    reg.entry(session_id).or_default().push(sink);
}

pub fn register_closure_subscriber(session_id: impl Into<String>, closure: VmValue) {
    let session_id = session_id.into();
    CLOSURE_SUBSCRIBERS.with(|reg| {
        reg.borrow_mut()
            .entry(session_id)
            .or_default()
            .push(closure);
    });
}

pub fn closure_subscribers_for(session_id: &str) -> Vec<VmValue> {
    CLOSURE_SUBSCRIBERS.with(|reg| reg.borrow().get(session_id).cloned().unwrap_or_default())
}

pub fn clear_session_sinks(session_id: &str) {
    external_sinks()
        .write()
        .expect("sink registry poisoned")
        .remove(session_id);
    CLOSURE_SUBSCRIBERS.with(|reg| {
        reg.borrow_mut().remove(session_id);
    });
}

pub fn reset_all_sinks() {
    external_sinks()
        .write()
        .expect("sink registry poisoned")
        .clear();
    CLOSURE_SUBSCRIBERS.with(|reg| {
        reg.borrow_mut().clear();
    });
}

/// Emit an event to external sinks registered for this session. Pipeline
/// closure subscribers are NOT called by this function — the agent
/// loop owns that path because it needs its async VM context.
pub fn emit_event(event: &AgentEvent) {
    let sinks: Vec<Arc<dyn AgentEventSink>> = {
        let reg = external_sinks().read().expect("sink registry poisoned");
        reg.get(event.session_id()).cloned().unwrap_or_default()
    };
    for sink in sinks {
        sink.handle_event(event);
    }
}

pub fn session_external_sink_count(session_id: &str) -> usize {
    external_sinks()
        .read()
        .expect("sink registry poisoned")
        .get(session_id)
        .map(|v| v.len())
        .unwrap_or(0)
}

pub fn session_closure_subscriber_count(session_id: &str) -> usize {
    CLOSURE_SUBSCRIBERS.with(|reg| reg.borrow().get(session_id).map(|v| v.len()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingSink(Arc<AtomicUsize>);
    impl AgentEventSink for CountingSink {
        fn handle_event(&self, _event: &AgentEvent) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn multi_sink_fans_out_in_order() {
        let multi = MultiSink::new();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        multi.push(Arc::new(CountingSink(a.clone())));
        multi.push(Arc::new(CountingSink(b.clone())));
        let event = AgentEvent::TurnStart {
            session_id: "s1".into(),
            iteration: 1,
        };
        multi.handle_event(&event);
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn session_scoped_sink_routing() {
        reset_all_sinks();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        register_sink("session-a", Arc::new(CountingSink(a.clone())));
        register_sink("session-b", Arc::new(CountingSink(b.clone())));
        emit_event(&AgentEvent::TurnStart {
            session_id: "session-a".into(),
            iteration: 0,
        });
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 0);
        emit_event(&AgentEvent::TurnEnd {
            session_id: "session-b".into(),
            iteration: 0,
            turn_info: serde_json::json!({}),
        });
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 1);
        clear_session_sinks("session-a");
        assert_eq!(session_external_sink_count("session-a"), 0);
        assert_eq!(session_external_sink_count("session-b"), 1);
        reset_all_sinks();
    }

    #[test]
    fn tool_call_status_serde() {
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::Failed).unwrap(),
            "\"failed\""
        );
    }
}
