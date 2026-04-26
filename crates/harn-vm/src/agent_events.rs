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

use crate::event_log::{AnyEventLog, EventLog, LogEvent as EventLogRecord, Topic};
use crate::orchestration::HandoffArtifact;
use crate::tool_annotations::ToolKind;

/// One coalesced filesystem notification from a hostlib `fs_watch`
/// subscription.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FsWatchEvent {
    pub kind: String,
    pub paths: Vec<String>,
    pub relative_paths: Vec<String>,
    pub raw_kind: String,
    pub error: Option<String>,
}

/// Typed worker lifecycle events emitted by delegated/background agent
/// execution. Bridge-facing worker updates still derive a string status
/// from these variants, but the runtime no longer passes raw status
/// strings around internally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum WorkerEvent {
    WorkerSpawned,
    WorkerCompleted,
    WorkerFailed,
    WorkerCancelled,
}

impl WorkerEvent {
    pub fn as_status(self) -> &'static str {
        match self {
            Self::WorkerSpawned => "running",
            Self::WorkerCompleted => "completed",
            Self::WorkerFailed => "failed",
            Self::WorkerCancelled => "cancelled",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkerSpawned => "WorkerSpawned",
            Self::WorkerCompleted => "WorkerCompleted",
            Self::WorkerFailed => "WorkerFailed",
            Self::WorkerCancelled => "WorkerCancelled",
        }
    }
}

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

/// Wire-level classification of a `ToolCallUpdate` failure. Pairs with the
/// human-readable `error` string so clients can render each failure type
/// distinctly (e.g. surface a "permission denied" badge, or a different
/// retry affordance for `network` vs `tool_error`). The enum is
/// deliberately extensible — `unknown` is the default when the runtime
/// could not classify a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallErrorCategory {
    /// Host-side validation rejected the args (missing required field,
    /// invalid type, malformed JSON).
    SchemaValidation,
    /// The tool ran and returned an error result (e.g. `read_file` on a
    /// missing path) — distinguished from a transport failure.
    ToolError,
    /// MCP transport / server-protocol error.
    McpServerError,
    /// Burin Swift host bridge returned an error during dispatch.
    HostBridgeError,
    /// `session/request_permission` denied by the client, or a policy
    /// rule (static or dynamic) refused the call.
    PermissionDenied,
    /// The harn loop detector skipped this call because the same
    /// (tool, args) pair repeated past the configured threshold.
    RejectedLoop,
    /// Streaming text candidate was detected (bare `name(` or
    /// `<tool_call>` opener) but never resolved into a parseable call:
    /// args parsed as malformed, the heredoc body broke, the tag closed
    /// without a balanced expression, or the stream ended mid-call.
    /// Used by the streaming candidate detector (harn#692) to retract a
    /// `tool_call` candidate that turned out to be prose or syntactically
    /// broken so clients can dismiss the in-flight chip.
    ParseAborted,
    /// The tool exceeded its time budget.
    Timeout,
    /// Transient network / rate-limited / 5xx provider failure.
    Network,
    /// The tool was cancelled (e.g. session aborted).
    Cancelled,
    /// Default when classification was not performed.
    Unknown,
}

impl ToolCallErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema_validation",
            Self::ToolError => "tool_error",
            Self::McpServerError => "mcp_server_error",
            Self::HostBridgeError => "host_bridge_error",
            Self::PermissionDenied => "permission_denied",
            Self::RejectedLoop => "rejected_loop",
            Self::ParseAborted => "parse_aborted",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// Map an internal `ErrorCategory` (used by the VM's `VmError`
    /// classification) onto the wire enum. The internal taxonomy is
    /// finer-grained — several transient categories collapse onto
    /// `Network`, and the auth/quota family becomes `HostBridgeError`
    /// because at the tool-dispatch boundary those errors come from
    /// the bridge transport rather than the tool itself.
    pub fn from_internal(category: &crate::value::ErrorCategory) -> Self {
        use crate::value::ErrorCategory as Internal;
        match category {
            Internal::Timeout => Self::Timeout,
            Internal::RateLimit
            | Internal::Overloaded
            | Internal::ServerError
            | Internal::TransientNetwork => Self::Network,
            Internal::SchemaValidation => Self::SchemaValidation,
            Internal::ToolError => Self::ToolError,
            Internal::ToolRejected => Self::PermissionDenied,
            Internal::Cancelled => Self::Cancelled,
            Internal::Auth
            | Internal::EgressBlocked
            | Internal::NotFound
            | Internal::CircuitOpen
            | Internal::Generic => Self::HostBridgeError,
        }
    }
}

/// Where a tool actually ran. Tags `ToolCallUpdate` so clients can render
/// "via mcp:linear" / "via host bridge" badges, attribute latency by
/// transport, and route errors to the right surface (harn#691).
///
/// On the wire this serializes adjacently-tagged so the `mcp_server`
/// case carries the configured server name. The ACP adapter rewrites
/// unit variants as bare strings (`"harn_builtin"`, `"host_bridge"`,
/// `"provider_native"`) and the `McpServer` case as
/// `{"kind": "mcp_server", "serverName": "..."}` to match the protocol's
/// camelCase convention.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolExecutor {
    /// VM-stdlib (`read_file`, `write_file`, `exec`, `http_*`, `mcp_*`)
    /// or any Harn-side handler closure registered in `tools_val`.
    HarnBuiltin,
    /// Capability provided by the host through `HostBridge.builtin_call`
    /// (Swift-side IDE bridge, BurinApp, BurinCLI host shells).
    HostBridge,
    /// Tool dispatched against a configured MCP server. Detected by the
    /// `_mcp_server` tag that `mcp_list_tools` injects on every tool
    /// dict before the agent loop sees it.
    McpServer { server_name: String },
    /// Provider-side server-side tool execution — currently OpenAI
    /// Responses-API server tools (e.g. native `tool_search`). The
    /// runtime never dispatches these locally; the model returns the
    /// already-executed result inline.
    ProviderNative,
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
        /// Set to `Some(true)` by the streaming candidate detector
        /// (harn#692) when this event represents a tool-call shape
        /// detected in the model's in-flight assistant text but whose
        /// arguments have not finished parsing yet. Clients can render a
        /// spinner / placeholder while the model writes the body. The
        /// detector follows up with a `ToolCallUpdate { parsing: false,
        /// .. }` carrying either `status: pending` (promoted) or
        /// `status: failed` with `error_category: parse_aborted`.
        /// `None` (the default) means "this is a normal post-parse tool
        /// call, no candidate phase was active" so the on-disk shape
        /// stays compatible with replays recorded before this field
        /// existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsing: Option<bool>,
    },
    ToolCallUpdate {
        session_id: String,
        tool_call_id: String,
        tool_name: String,
        status: ToolCallStatus,
        raw_output: Option<serde_json::Value>,
        error: Option<String>,
        /// Wall-clock milliseconds from the parse-to-execution boundary
        /// to the terminal `Completed`/`Failed` update. Includes the
        /// time spent in any wrapping orchestration logic (loop checks,
        /// post-tool hooks, microcompaction). Populated only on the
        /// terminal update — `None` on intermediate `Pending` /
        /// `InProgress` updates so clients can ignore the field until
        /// it shows up.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Milliseconds spent in the actual host/builtin/MCP dispatch
        /// call only (the inner `dispatch_tool_execution` window).
        /// Populated only on the terminal update; `None` otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_duration_ms: Option<u64>,
        /// Structured classification of the failure (when `status` is
        /// `Failed`). Paired with `error` so clients can render each
        /// category distinctly without parsing free-form strings. Always
        /// `None` for non-Failed updates and serialized as
        /// `errorCategory` in the ACP wire format.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_category: Option<ToolCallErrorCategory>,
        /// Where the tool actually ran. `None` only for events emitted
        /// from sites that pre-date the dispatch decision (e.g. the
        /// pending → in-progress transition the loop emits before the
        /// dispatcher picks a backend).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executor: Option<ToolExecutor>,
        /// Companion to `ToolCall.parsing` (harn#692). The streaming
        /// candidate detector emits the *terminal* candidate event as a
        /// `ToolCallUpdate` with `parsing: Some(false)` to retract the
        /// in-flight `parsing: true` chip — either by promoting the
        /// candidate (`status: pending`, populated `raw_output: None`,
        /// `error: None`) or aborting it (`status: failed`,
        /// `error_category: parse_aborted`). `None` means this update is
        /// not part of a candidate-phase transition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parsing: Option<bool>,
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
    /// Emitted when a skill is activated. Carries the match reason so
    /// replayers can reconstruct *why* a given skill took effect at
    /// this iteration.
    SkillActivated {
        session_id: String,
        skill_name: String,
        iteration: usize,
        reason: String,
    },
    /// Emitted when a previously-active skill is deactivated because
    /// the reassess phase no longer matches it.
    SkillDeactivated {
        session_id: String,
        skill_name: String,
        iteration: usize,
    },
    /// Emitted once per activation when the skill's `allowed_tools` filter
    /// narrows the effective tool surface exposed to the model.
    SkillScopeTools {
        session_id: String,
        skill_name: String,
        allowed_tools: Vec<String>,
    },
    /// Emitted when a `tool_search` query is issued by the model. Carries
    /// the raw query args, the configured strategy, and a `mode` tag
    /// distinguishing the client-executed fallback (`"client"`) from
    /// provider-native paths (`"anthropic"` / `"openai"`). Mirrors the
    /// transcript event shape so hosts can render a search-in-progress
    /// chip in real time — the replay path walks the transcript after
    /// the turn, which is too late for live UX.
    ToolSearchQuery {
        session_id: String,
        tool_use_id: String,
        name: String,
        query: serde_json::Value,
        strategy: String,
        mode: String,
    },
    /// Emitted when `tool_search` resolves — carries the list of tool
    /// names newly promoted into the model's effective surface for the
    /// next turn. Pair-emitted with `ToolSearchQuery` on every search.
    ToolSearchResult {
        session_id: String,
        tool_use_id: String,
        promoted: Vec<String>,
        strategy: String,
        mode: String,
    },
    TranscriptCompacted {
        session_id: String,
        mode: String,
        strategy: String,
        archived_messages: usize,
        estimated_tokens_before: usize,
        estimated_tokens_after: usize,
        snapshot_asset_id: Option<String>,
    },
    Handoff {
        session_id: String,
        artifact_id: String,
        handoff: Box<HandoffArtifact>,
    },
    FsWatch {
        session_id: String,
        subscription_id: String,
        events: Vec<FsWatchEvent>,
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
            | Self::DaemonWatchdogTripped { session_id, .. }
            | Self::SkillActivated { session_id, .. }
            | Self::SkillDeactivated { session_id, .. }
            | Self::SkillScopeTools { session_id, .. }
            | Self::ToolSearchQuery { session_id, .. }
            | Self::ToolSearchResult { session_id, .. }
            | Self::TranscriptCompacted { session_id, .. }
            | Self::Handoff { session_id, .. }
            | Self::FsWatch { session_id, .. } => session_id,
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

/// Event-log-backed sink for a single session's agent event stream.
/// Uses the generalized append-only event log when one is installed for
/// the current VM thread and falls back to `JsonlEventSink` only for
/// older env-driven workflows.
pub struct EventLogSink {
    log: Arc<AnyEventLog>,
    topic: Topic,
    session_id: String,
}

impl EventLogSink {
    pub fn new(log: Arc<AnyEventLog>, session_id: impl Into<String>) -> Arc<Self> {
        let session_id = session_id.into();
        let topic = Topic::new(format!(
            "observability.agent_events.{}",
            crate::event_log::sanitize_topic_component(&session_id)
        ))
        .expect("session id should sanitize to a valid topic");
        Arc::new(Self {
            log,
            topic,
            session_id,
        })
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

impl AgentEventSink for EventLogSink {
    fn handle_event(&self, event: &AgentEvent) {
        let event_json = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(_) => return,
        };
        let event_kind = event_json
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("agent_event")
            .to_string();
        let payload = serde_json::json!({
            "index_hint": now_ms(),
            "session_id": self.session_id,
            "event": event_json,
        });
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("session_id".to_string(), self.session_id.clone());
        let log = self.log.clone();
        let topic = self.topic.clone();
        let record = EventLogRecord::new(event_kind, payload).with_headers(headers);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = log.append(&topic, record).await;
            });
        } else {
            let _ = futures::executor::block_on(log.append(&topic, record));
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
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
    fn tool_call_update_durations_serialize_when_present_and_skip_when_absent() {
        // Terminal update with both durations populated — both fields
        // appear in the JSON. Snake_case keys here because this is the
        // canonical AgentEvent shape; the ACP adapter renames to
        // camelCase separately.
        let terminal = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "tc-1".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: Some(42),
            execution_duration_ms: Some(7),
            error_category: None,
            executor: None,
            parsing: None,
        };
        let value = serde_json::to_value(&terminal).unwrap();
        assert_eq!(value["duration_ms"], serde_json::json!(42));
        assert_eq!(value["execution_duration_ms"], serde_json::json!(7));

        // In-progress update with `None` for both — both keys must be
        // absent (not `null`) so older ACP clients that key off
        // presence don't see a misleading zero.
        let intermediate = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "tc-1".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::InProgress,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            parsing: None,
        };
        let value = serde_json::to_value(&intermediate).unwrap();
        let object = value.as_object().expect("update serializes as object");
        assert!(
            !object.contains_key("duration_ms"),
            "duration_ms must be omitted when None: {value}"
        );
        assert!(
            !object.contains_key("execution_duration_ms"),
            "execution_duration_ms must be omitted when None: {value}"
        );
    }

    #[test]
    fn tool_call_update_deserializes_without_duration_fields_for_back_compat() {
        // Persisted event-log entries written before the fields existed
        // must still deserialize cleanly. The missing keys map to None.
        let raw = serde_json::json!({
            "type": "tool_call_update",
            "session_id": "s",
            "tool_call_id": "tc-1",
            "tool_name": "read",
            "status": "completed",
            "raw_output": null,
            "error": null,
        });
        let event: AgentEvent = serde_json::from_value(raw).expect("parses without duration keys");
        match event {
            AgentEvent::ToolCallUpdate {
                duration_ms,
                execution_duration_ms,
                ..
            } => {
                assert!(duration_ms.is_none());
                assert!(execution_duration_ms.is_none());
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
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

    #[test]
    fn tool_call_error_category_serializes_as_snake_case() {
        let pairs = [
            (ToolCallErrorCategory::SchemaValidation, "schema_validation"),
            (ToolCallErrorCategory::ToolError, "tool_error"),
            (ToolCallErrorCategory::McpServerError, "mcp_server_error"),
            (ToolCallErrorCategory::HostBridgeError, "host_bridge_error"),
            (ToolCallErrorCategory::PermissionDenied, "permission_denied"),
            (ToolCallErrorCategory::RejectedLoop, "rejected_loop"),
            (ToolCallErrorCategory::ParseAborted, "parse_aborted"),
            (ToolCallErrorCategory::Timeout, "timeout"),
            (ToolCallErrorCategory::Network, "network"),
            (ToolCallErrorCategory::Cancelled, "cancelled"),
            (ToolCallErrorCategory::Unknown, "unknown"),
        ];
        for (variant, wire) in pairs {
            let encoded = serde_json::to_string(&variant).unwrap();
            assert_eq!(encoded, format!("\"{wire}\""));
            assert_eq!(variant.as_str(), wire);
            // Round-trip via deserialize so wire stability is enforced
            // both ways.
            let decoded: ToolCallErrorCategory = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn tool_executor_round_trips_with_adjacent_tag() {
        // Adjacent tagging keeps the wire shape uniform — every variant
        // is a JSON object with a `kind` discriminator. The ACP adapter
        // rewrites unit variants as bare strings; the on-disk event log
        // keeps the object shape so deserialize can recover the variant.
        for executor in [
            ToolExecutor::HarnBuiltin,
            ToolExecutor::HostBridge,
            ToolExecutor::McpServer {
                server_name: "linear".to_string(),
            },
            ToolExecutor::ProviderNative,
        ] {
            let json = serde_json::to_value(&executor).unwrap();
            let kind = json.get("kind").and_then(|v| v.as_str()).unwrap();
            match &executor {
                ToolExecutor::HarnBuiltin => assert_eq!(kind, "harn_builtin"),
                ToolExecutor::HostBridge => assert_eq!(kind, "host_bridge"),
                ToolExecutor::McpServer { server_name } => {
                    assert_eq!(kind, "mcp_server");
                    assert_eq!(json["server_name"], *server_name);
                }
                ToolExecutor::ProviderNative => assert_eq!(kind, "provider_native"),
            }
            let recovered: ToolExecutor = serde_json::from_value(json).unwrap();
            assert_eq!(recovered, executor);
        }
    }

    #[test]
    fn tool_call_error_category_from_internal_collapses_transient_family() {
        use crate::value::ErrorCategory as Internal;
        assert_eq!(
            ToolCallErrorCategory::from_internal(&Internal::Timeout),
            ToolCallErrorCategory::Timeout
        );
        for net in [
            Internal::RateLimit,
            Internal::Overloaded,
            Internal::ServerError,
            Internal::TransientNetwork,
        ] {
            assert_eq!(
                ToolCallErrorCategory::from_internal(&net),
                ToolCallErrorCategory::Network,
                "{net:?} should map to Network",
            );
        }
        assert_eq!(
            ToolCallErrorCategory::from_internal(&Internal::SchemaValidation),
            ToolCallErrorCategory::SchemaValidation
        );
        assert_eq!(
            ToolCallErrorCategory::from_internal(&Internal::ToolError),
            ToolCallErrorCategory::ToolError
        );
        assert_eq!(
            ToolCallErrorCategory::from_internal(&Internal::ToolRejected),
            ToolCallErrorCategory::PermissionDenied
        );
        assert_eq!(
            ToolCallErrorCategory::from_internal(&Internal::Cancelled),
            ToolCallErrorCategory::Cancelled
        );
        for bridge in [
            Internal::Auth,
            Internal::EgressBlocked,
            Internal::NotFound,
            Internal::CircuitOpen,
            Internal::Generic,
        ] {
            assert_eq!(
                ToolCallErrorCategory::from_internal(&bridge),
                ToolCallErrorCategory::HostBridgeError,
                "{bridge:?} should map to HostBridgeError",
            );
        }
    }

    #[test]
    fn tool_call_update_event_omits_error_category_when_none() {
        let event = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "t".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            parsing: None,
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["type"], "tool_call_update");
        assert!(v.get("error_category").is_none());
    }

    #[test]
    fn tool_call_update_event_serializes_error_category_when_set() {
        let event = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "t".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some("missing required field".into()),
            duration_ms: None,
            execution_duration_ms: None,
            error_category: Some(ToolCallErrorCategory::SchemaValidation),
            executor: None,
            parsing: None,
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["error_category"], "schema_validation");
        assert_eq!(v["error"], "missing required field");
    }

    #[test]
    fn tool_call_update_omits_executor_when_absent() {
        // `executor: None` must not appear in the serialized event so
        // the on-disk shape stays backward-compatible with replays
        // recorded before harn#691.
        let event = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "tc-1".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            parsing: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("executor").is_none(), "got: {json}");
    }

    #[test]
    fn tool_call_update_includes_executor_when_present() {
        let event = AgentEvent::ToolCallUpdate {
            session_id: "s".into(),
            tool_call_id: "tc-1".into(),
            tool_name: "read".into(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: Some(ToolExecutor::McpServer {
                server_name: "github".into(),
            }),
            parsing: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["executor"]["kind"], "mcp_server");
        assert_eq!(json["executor"]["server_name"], "github");
    }
}
