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
//!    Stored in a global `OnceLock<RwLock<HashMap<...>>>` here.
//! 2. **Closure subscribers** — `.harn` closures registered via the
//!    `agent_subscribe(session_id, callback)` host builtin. These live
//!    on the session's `SessionState.subscribers` in
//!    `crate::agent_sessions`, because sessions are the single source
//!    of truth for session-scoped VM state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::tool_annotations::ToolKind;

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

/// Envelope written to `event_log.jsonl` (#103). Wraps the raw
/// `AgentEvent` with monotonic index + timestamp + frame depth so
/// replay engines can reconstruct paused state at any event index,
/// and scrubber UIs can bucket events by time. The envelope is the
/// on-disk shape; the wire format for live consumers is still the
/// raw `AgentEvent` so existing sinks don't churn.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedAgentEvent {
    /// Monotonic per-session index starting at 0. Unique within a
    /// session; gaps never happen even under load because the sink
    /// owns the counter under a mutex.
    pub index: u64,
    /// Milliseconds since the Unix epoch, captured when the sink
    /// received the event. Not the event's emission time — that
    /// would require threading a clock through every emit site.
    pub emitted_at_ms: i64,
    /// Call-stack depth at the moment of emission, when the caller
    /// can supply it. `None` for events emitted from a context where
    /// the VM frame stack isn't available.
    pub frame_depth: Option<u32>,
    /// The raw event, flattened so `jq '.type'` works as expected.
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// Append-only JSONL sink for a single session's event stream (#103).
/// One writer per session; sinks rotate to a numbered suffix when a
/// running file crosses `ROTATE_BYTES` (100 MB today — long chat
/// sessions rarely exceed 5 MB, so rotation almost never fires).
pub struct JsonlEventSink {
    state: Mutex<JsonlEventSinkState>,
    base_path: std::path::PathBuf,
}

struct JsonlEventSinkState {
    writer: std::io::BufWriter<std::fs::File>,
    index: u64,
    bytes_written: u64,
    rotation: u32,
}

impl JsonlEventSink {
    /// Hard cap past which the current file rotates to a numbered
    /// suffix (`event_log-000001.jsonl`). Chosen so long debugging
    /// sessions don't produce unreadable multi-GB logs.
    pub const ROTATE_BYTES: u64 = 100 * 1024 * 1024;

    /// Open a new sink writing to `base_path`. Creates parent dirs
    /// if missing. Overwrites an existing file so each fresh session
    /// starts from index 0.
    pub fn open(base_path: impl Into<std::path::PathBuf>) -> std::io::Result<Arc<Self>> {
        let base_path = base_path.into();
        if let Some(parent) = base_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&base_path)?;
        Ok(Arc::new(Self {
            state: Mutex::new(JsonlEventSinkState {
                writer: std::io::BufWriter::new(file),
                index: 0,
                bytes_written: 0,
                rotation: 0,
            }),
            base_path,
        }))
    }

    /// Flush any buffered writes. Called on session shutdown; the
    /// Drop impl calls this too but on early panic it may not run.
    pub fn flush(&self) -> std::io::Result<()> {
        use std::io::Write as _;
        self.state
            .lock()
            .expect("jsonl sink mutex poisoned")
            .writer
            .flush()
    }

    /// Current event index — primarily for tests and the "how many
    /// events are in this run" run-record summary.
    pub fn event_count(&self) -> u64 {
        self.state.lock().expect("jsonl sink mutex poisoned").index
    }

    fn rotate_if_needed(&self, state: &mut JsonlEventSinkState) -> std::io::Result<()> {
        use std::io::Write as _;
        if state.bytes_written < Self::ROTATE_BYTES {
            return Ok(());
        }
        state.writer.flush()?;
        state.rotation += 1;
        let suffix = format!("-{:06}", state.rotation);
        let rotated = self.base_path.with_file_name({
            let stem = self
                .base_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("event_log");
            let ext = self
                .base_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jsonl");
            format!("{stem}{suffix}.{ext}")
        });
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&rotated)?;
        state.writer = std::io::BufWriter::new(file);
        state.bytes_written = 0;
        Ok(())
    }
}

impl AgentEventSink for JsonlEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        use std::io::Write as _;
        let mut state = self.state.lock().expect("jsonl sink mutex poisoned");
        let index = state.index;
        state.index += 1;
        let emitted_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let envelope = PersistedAgentEvent {
            index,
            emitted_at_ms,
            frame_depth: None,
            event: event.clone(),
        };
        if let Ok(line) = serde_json::to_string(&envelope) {
            // One line, newline-terminated — JSON Lines spec.
            // Errors here are swallowed on purpose; a failing write
            // must never crash the agent loop, and the run record
            // itself is a secondary artifact.
            let _ = state.writer.write_all(line.as_bytes());
            let _ = state.writer.write_all(b"\n");
            state.bytes_written += line.len() as u64 + 1;
            let _ = self.rotate_if_needed(&mut state);
        }
    }
}

impl Drop for JsonlEventSink {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            use std::io::Write as _;
            let _ = state.writer.flush();
        }
    }
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

type ExternalSinkRegistry = RwLock<HashMap<String, Vec<Arc<dyn AgentEventSink>>>>;

fn external_sinks() -> &'static ExternalSinkRegistry {
    static REGISTRY: OnceLock<ExternalSinkRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_sink(session_id: impl Into<String>, sink: Arc<dyn AgentEventSink>) {
    let session_id = session_id.into();
    let mut reg = external_sinks().write().expect("sink registry poisoned");
    reg.entry(session_id).or_default().push(sink);
}

/// Remove all external sinks registered for `session_id`. Does NOT
/// close the session itself — subscribers and transcript survive, so a
/// later `agent_loop` call with the same id continues the conversation.
pub fn clear_session_sinks(session_id: &str) {
    external_sinks()
        .write()
        .expect("sink registry poisoned")
        .remove(session_id);
}

pub fn reset_all_sinks() {
    external_sinks()
        .write()
        .expect("sink registry poisoned")
        .clear();
    crate::agent_sessions::reset_session_store();
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
    crate::agent_sessions::subscriber_count(session_id)
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
    fn jsonl_sink_writes_monotonic_indices_and_timestamps() {
        use std::io::{BufRead, BufReader};
        let dir = std::env::temp_dir().join(format!("harn-event-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("event_log.jsonl");
        let sink = JsonlEventSink::open(&path).unwrap();
        for i in 0..5 {
            sink.handle_event(&AgentEvent::TurnStart {
                session_id: "s".into(),
                iteration: i,
            });
        }
        assert_eq!(sink.event_count(), 5);
        sink.flush().unwrap();

        // Read back + assert monotonic indices + non-decreasing timestamps.
        let file = std::fs::File::open(&path).unwrap();
        let mut last_idx: i64 = -1;
        let mut last_ts: i64 = 0;
        for line in BufReader::new(file).lines() {
            let line = line.unwrap();
            let val: serde_json::Value = serde_json::from_str(&line).unwrap();
            let idx = val["index"].as_i64().unwrap();
            let ts = val["emitted_at_ms"].as_i64().unwrap();
            assert_eq!(idx, last_idx + 1, "indices must be contiguous");
            assert!(ts >= last_ts, "timestamps must be non-decreasing");
            last_idx = idx;
            last_ts = ts;
            // Event payload flattened — type tag must survive.
            assert_eq!(val["type"], "turn_start");
        }
        assert_eq!(last_idx, 4);
        let _ = std::fs::remove_file(&path);
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
