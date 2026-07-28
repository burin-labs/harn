use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::event_log::{AnyEventLog, EventLog, LogEvent as EventLogRecord, Topic};

use super::AgentEvent;

/// External consumers of the event stream (e.g. the harn-cli ACP server,
/// which translates events into JSON-RPC notifications).
pub trait AgentEventSink: Send + Sync {
    fn handle_event(&self, event: &AgentEvent);

    /// Wait until every event accepted before this call has reached the sink's
    /// durable boundary. Synchronous sinks are complete when `handle_event`
    /// returns, so their default barrier is immediately ready.
    fn flush(&self) -> AgentEventSinkFlush<'_> {
        Box::pin(async { Ok(()) })
    }
}

pub type AgentEventSinkFlush<'a> =
    Pin<Box<dyn Future<Output = Result<(), AgentEventSinkError>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEventSinkError {
    sink: &'static str,
    message: String,
    dropped_events: u64,
}

impl AgentEventSinkError {
    pub fn new(sink: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            sink,
            message: error.to_string(),
            dropped_events: 0,
        }
    }

    fn dropped_event(sink: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            sink,
            message: error.to_string(),
            dropped_events: 1,
        }
    }

    pub fn sink(&self) -> &'static str {
        self.sink
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Number of emitted events known not to have crossed the durable boundary.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

impl std::fmt::Display for AgentEventSinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} sink flush failed: {}", self.sink, self.message)?;
        if self.dropped_events > 0 {
            write!(f, " ({} dropped events)", self.dropped_events)?;
        }
        Ok(())
    }
}

impl std::error::Error for AgentEventSinkError {}

fn record_sink_failure(first_error: &mut Option<AgentEventSinkError>, error: AgentEventSinkError) {
    if let Some(first) = first_error {
        first.dropped_events = first.dropped_events.saturating_add(error.dropped_events);
    } else {
        *first_error = Some(error);
    }
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
    first_error: Option<AgentEventSinkError>,
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
                first_error: None,
            }),
            base_path,
        }))
    }

    /// Flush any buffered writes. Called on session shutdown; the
    /// Drop impl calls this too but on early panic it may not run.
    pub fn flush(&self) -> Result<(), AgentEventSinkError> {
        use std::io::Write as _;
        let mut state = self.state.lock().expect("jsonl sink mutex poisoned");
        if let Err(error) = state.writer.flush() {
            record_sink_failure(
                &mut state.first_error,
                AgentEventSinkError::new("jsonl_event", error),
            );
        }
        state.first_error.clone().map_or(Ok(()), Err)
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
    dispatch: EventLogSinkDispatch,
    session_id: String,
    first_error: Arc<Mutex<Option<AgentEventSinkError>>>,
}

enum EventLogSinkDispatch {
    Async(tokio::sync::mpsc::UnboundedSender<EventLogSinkCommand>),
    Blocking { log: Arc<AnyEventLog>, topic: Topic },
}

enum EventLogSinkCommand {
    Append(EventLogRecord),
    Flush(tokio::sync::oneshot::Sender<Result<(), AgentEventSinkError>>),
}

impl EventLogSink {
    pub fn new(log: Arc<AnyEventLog>, session_id: impl Into<String>) -> Arc<Self> {
        let session_id = session_id.into();
        let topic = Topic::new(format!(
            "observability.agent_events.{}",
            crate::event_log::sanitize_topic_component(&session_id)
        ))
        .expect("session id should sanitize to a valid topic");
        let first_error = Arc::new(Mutex::new(None));
        let dispatch = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            handle.spawn(run_event_log_sink_worker(
                log,
                topic,
                receiver,
                first_error.clone(),
            ));
            EventLogSinkDispatch::Async(sender)
        } else {
            EventLogSinkDispatch::Blocking { log, topic }
        };
        Arc::new(Self {
            dispatch,
            session_id,
            first_error,
        })
    }

    pub async fn flush(&self) -> Result<(), AgentEventSinkError> {
        match &self.dispatch {
            EventLogSinkDispatch::Async(sender) => {
                let (reply, response) = tokio::sync::oneshot::channel();
                if sender.send(EventLogSinkCommand::Flush(reply)).is_err() {
                    return Err(self.latched_error().unwrap_or_else(|| {
                        AgentEventSinkError::new("event_log", "append worker is unavailable")
                    }));
                }
                response.await.unwrap_or_else(|_| {
                    Err(self.latched_error().unwrap_or_else(|| {
                        AgentEventSinkError::new("event_log", "append worker stopped before flush")
                    }))
                })
            }
            EventLogSinkDispatch::Blocking { log, .. } => {
                let append_error = self.latched_error();
                let flush_result = flush_event_log(log.clone()).await;
                append_error.map_or(flush_result, Err)
            }
        }
    }

    fn latched_error(&self) -> Option<AgentEventSinkError> {
        self.first_error
            .lock()
            .expect("event-log sink error mutex poisoned")
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn enqueue_flush_for_test(
        &self,
    ) -> tokio::sync::oneshot::Receiver<Result<(), AgentEventSinkError>> {
        let EventLogSinkDispatch::Async(sender) = &self.dispatch else {
            panic!("test flush enqueue requires an async event-log sink");
        };
        let (reply, response) = tokio::sync::oneshot::channel();
        sender
            .send(EventLogSinkCommand::Flush(reply))
            .expect("event-log sink worker should accept test flush");
        response
    }
}

async fn run_event_log_sink_worker(
    log: Arc<AnyEventLog>,
    topic: Topic,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<EventLogSinkCommand>,
    first_error: Arc<Mutex<Option<AgentEventSinkError>>>,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            EventLogSinkCommand::Append(record) => {
                if let Err(error) = log.append(&topic, record).await {
                    record_sink_failure(
                        &mut first_error
                            .lock()
                            .expect("event-log sink error mutex poisoned"),
                        AgentEventSinkError::dropped_event("event_log", error),
                    );
                }
            }
            EventLogSinkCommand::Flush(reply) => {
                let flush_result = flush_event_log(log.clone()).await;
                let append_error = first_error
                    .lock()
                    .expect("event-log sink error mutex poisoned")
                    .clone();
                let result = append_error.map_or(flush_result, Err);
                let _ = reply.send(result);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EventLogFlushSchedule {
    AsyncExecutor,
    BlockingPool,
}

pub(super) fn event_log_flush_schedule(log: &AnyEventLog) -> EventLogFlushSchedule {
    if matches!(log, AnyEventLog::Sqlite(_)) {
        EventLogFlushSchedule::BlockingPool
    } else {
        EventLogFlushSchedule::AsyncExecutor
    }
}

async fn flush_event_log(log: Arc<AnyEventLog>) -> Result<(), AgentEventSinkError> {
    let result = if event_log_flush_schedule(&log) == EventLogFlushSchedule::BlockingPool
        && tokio::runtime::Handle::try_current().is_ok()
    {
        tokio::task::spawn_blocking(move || futures::executor::block_on(log.flush()))
            .await
            .map_err(|error| {
                AgentEventSinkError::new("event_log", format!("flush task failed: {error}"))
            })?
    } else {
        log.flush().await
    };
    result.map_err(|error| AgentEventSinkError::new("event_log", error))
}

impl AgentEventSink for JsonlEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        use std::io::Write as _;
        let mut state = self.state.lock().expect("jsonl sink mutex poisoned");
        if state.first_error.is_some() {
            let error = AgentEventSinkError::dropped_event(
                "jsonl_event",
                "sink unavailable after an earlier persistence failure",
            );
            record_sink_failure(&mut state.first_error, error);
            return;
        }
        let index = state.index;
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
        let mut envelope_json = match serde_json::to_value(&envelope) {
            Ok(value) => value,
            Err(error) => {
                record_sink_failure(
                    &mut state.first_error,
                    AgentEventSinkError::dropped_event("jsonl_event", error),
                );
                return;
            }
        };
        crate::redact::current_policy().redact_json_in_place(&mut envelope_json);
        let mut line = match serde_json::to_vec(&envelope_json) {
            Ok(line) => line,
            Err(error) => {
                record_sink_failure(
                    &mut state.first_error,
                    AgentEventSinkError::dropped_event("jsonl_event", error),
                );
                return;
            }
        };
        line.push(b'\n');
        if let Err(error) = state
            .writer
            .write_all(&line)
            .and_then(|_| state.writer.flush())
        {
            record_sink_failure(
                &mut state.first_error,
                AgentEventSinkError::dropped_event("jsonl_event", error),
            );
            return;
        }
        state.index += 1;
        state.bytes_written += line.len() as u64;
        if let Err(error) = self.rotate_if_needed(&mut state) {
            record_sink_failure(
                &mut state.first_error,
                AgentEventSinkError::new("jsonl_event", error),
            );
        }
    }

    fn flush(&self) -> AgentEventSinkFlush<'_> {
        Box::pin(async move { JsonlEventSink::flush(self) })
    }
}

impl AgentEventSink for EventLogSink {
    fn handle_event(&self, event: &AgentEvent) {
        let event_json = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(error) => {
                record_sink_failure(
                    &mut self
                        .first_error
                        .lock()
                        .expect("event-log sink error mutex poisoned"),
                    AgentEventSinkError::dropped_event("event_log", error),
                );
                return;
            }
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
        let mut record = EventLogRecord::new(event_kind, payload).with_headers(headers);
        record.redact_in_place(&crate::redact::current_policy());
        match &self.dispatch {
            EventLogSinkDispatch::Async(sender) => {
                if sender.send(EventLogSinkCommand::Append(record)).is_err() {
                    record_sink_failure(
                        &mut self
                            .first_error
                            .lock()
                            .expect("event-log sink error mutex poisoned"),
                        AgentEventSinkError::dropped_event(
                            "event_log",
                            "append worker is unavailable",
                        ),
                    );
                }
            }
            EventLogSinkDispatch::Blocking { log, topic } => {
                if let Err(error) = futures::executor::block_on(log.append(topic, record)) {
                    record_sink_failure(
                        &mut self
                            .first_error
                            .lock()
                            .expect("event-log sink error mutex poisoned"),
                        AgentEventSinkError::dropped_event("event_log", error),
                    );
                }
            }
        }
    }

    fn flush(&self) -> AgentEventSinkFlush<'_> {
        Box::pin(EventLogSink::flush(self))
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

    fn flush(&self) -> AgentEventSinkFlush<'_> {
        let sinks = self.sinks.lock().expect("sink mutex poisoned").clone();
        Box::pin(flush_all_sinks(sinks))
    }
}

pub(super) async fn flush_all_sinks(
    sinks: impl IntoIterator<Item = Arc<dyn AgentEventSink>>,
) -> Result<(), AgentEventSinkError> {
    let mut first_error = None;
    for sink in sinks {
        if let Err(error) = sink.flush().await {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
