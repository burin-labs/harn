use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::runtime_limits::RuntimeLimits;

mod file;
mod memory;
mod sqlite;
mod util;

#[cfg(test)]
mod tests;

pub use file::FileEventLog;
pub use memory::MemoryEventLog;
pub use sqlite::SqliteEventLog;

pub type EventId = u64;

pub const HARN_EVENT_LOG_BACKEND_ENV: &str = "HARN_EVENT_LOG_BACKEND";
pub const HARN_EVENT_LOG_DIR_ENV: &str = "HARN_EVENT_LOG_DIR";
pub const HARN_EVENT_LOG_SQLITE_PATH_ENV: &str = "HARN_EVENT_LOG_SQLITE_PATH";
pub const HARN_EVENT_LOG_QUEUE_DEPTH_ENV: &str = "HARN_EVENT_LOG_QUEUE_DEPTH";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Topic(String);

impl Topic {
    pub fn new(value: impl Into<String>) -> Result<Self, LogError> {
        let value = value.into();
        if value.is_empty() {
            return Err(LogError::InvalidTopic("topic cannot be empty".to_string()));
        }
        if !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            return Err(LogError::InvalidTopic(format!(
                "topic '{value}' contains unsupported characters"
            )));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Topic {
    type Err = LogError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ConsumerId(String);

impl ConsumerId {
    pub fn new(value: impl Into<String>) -> Result<Self, LogError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(LogError::InvalidConsumer(
                "consumer id cannot be empty".to_string(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConsumerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLogBackendKind {
    Memory,
    File,
    Sqlite,
}

impl fmt::Display for EventLogBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory => write!(f, "memory"),
            Self::File => write!(f, "file"),
            Self::Sqlite => write!(f, "sqlite"),
        }
    }
}

impl FromStr for EventLogBackendKind {
    type Err = LogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "file" => Ok(Self::File),
            "sqlite" => Ok(Self::Sqlite),
            other => Err(LogError::Config(format!(
                "unsupported event log backend '{other}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEvent {
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub occurred_at_ms: i64,
}

impl LogEvent {
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            headers: BTreeMap::new(),
            occurred_at_ms: util::now_ms(),
        }
    }

    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// Apply the unified redaction policy to this event's headers and
    /// payload. Backends are intentionally left unaware of redaction —
    /// emitters that need scrubbed events call this before append, and
    /// readers that materialize events for display can apply the policy
    /// again as defense in depth.
    pub fn redact_in_place(&mut self, policy: &crate::redact::RedactionPolicy) {
        self.headers = policy.redact_headers(&self.headers);
        policy.redact_json_in_place(&mut self.payload);
    }
}

/// Serialized event payload form for large read paths.
///
/// `payload` contains the original JSON bytes for backends that can expose
/// them directly. Callers that only need to forward or hash the payload can
/// avoid materializing a `serde_json::Value`; callers that need structured
/// access can opt in with `payload_json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEventBytes {
    pub kind: String,
    pub payload: Bytes,
    pub headers: BTreeMap<String, String>,
    pub occurred_at_ms: i64,
}

impl LogEventBytes {
    pub fn payload_json(&self) -> Result<serde_json::Value, LogError> {
        serde_json::from_slice(&self.payload)
            .map_err(|error| LogError::Serde(format!("event log payload parse error: {error}")))
    }

    pub fn into_log_event(self) -> Result<LogEvent, LogError> {
        Ok(LogEvent {
            kind: self.kind,
            payload: serde_json::from_slice(&self.payload).map_err(|error| {
                LogError::Serde(format!("event log payload parse error: {error}"))
            })?,
            headers: self.headers,
            occurred_at_ms: self.occurred_at_ms,
        })
    }
}

impl TryFrom<LogEvent> for LogEventBytes {
    type Error = LogError;

    fn try_from(event: LogEvent) -> Result<Self, Self::Error> {
        let payload = serde_json::to_vec(&event.payload)
            .map_err(|error| LogError::Serde(format!("event log payload encode error: {error}")))?;
        Ok(Self {
            kind: event.kind,
            payload: Bytes::from(payload),
            headers: event.headers,
            occurred_at_ms: event.occurred_at_ms,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactReport {
    pub removed: usize,
    pub remaining: usize,
    pub latest: Option<EventId>,
    pub checkpointed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendOutcome {
    pub event_id: EventId,
    pub event: LogEvent,
    pub inserted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLogDescription {
    pub backend: EventLogBackendKind,
    pub location: Option<PathBuf>,
    pub size_bytes: Option<u64>,
    pub queue_depth: usize,
}

#[derive(Debug)]
pub enum LogError {
    Config(String),
    InvalidTopic(String),
    InvalidConsumer(String),
    Io(String),
    Serde(String),
    Sqlite(String),
    ConsumerLagged(EventId),
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message)
            | Self::InvalidTopic(message)
            | Self::InvalidConsumer(message)
            | Self::Io(message)
            | Self::Serde(message)
            | Self::Sqlite(message) => message.fmt(f),
            Self::ConsumerLagged(last_id) => {
                write!(f, "subscriber lagged behind after event {last_id}")
            }
        }
    }
}

impl std::error::Error for LogError {}

#[allow(async_fn_in_trait)]
pub trait EventLog: Send + Sync {
    fn describe(&self) -> EventLogDescription;

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError>;

    async fn flush(&self) -> Result<(), LogError>;

    /// Read events strictly after `from`. `None` starts from the
    /// beginning of the topic.
    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError>;

    async fn read_range_bytes(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEventBytes)>, LogError> {
        let events = self.read_range(topic, from, limit).await?;
        events
            .into_iter()
            .map(|(event_id, event)| Ok((event_id, event.try_into()?)))
            .collect()
    }

    /// `async fn` keeps the ergonomic generic surface; the boxed stream
    /// preserves dyn-dispatch for callers that store `Arc<dyn EventLog>`.
    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError>;

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError>;

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError>;

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError>;

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError>;
}

#[derive(Clone, Debug)]
pub struct EventLogConfig {
    pub backend: EventLogBackendKind,
    pub file_dir: PathBuf,
    pub sqlite_path: PathBuf,
    pub queue_depth: usize,
}

impl EventLogConfig {
    pub fn for_base_dir(base_dir: &Path) -> Result<Self, LogError> {
        let backend = std::env::var(HARN_EVENT_LOG_BACKEND_ENV)
            .ok()
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(EventLogBackendKind::Sqlite);
        let queue_depth = std::env::var(HARN_EVENT_LOG_QUEUE_DEPTH_ENV)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(RuntimeLimits::DEFAULT.default_event_log_queue_depth)
            .max(1);

        let file_dir = match std::env::var(HARN_EVENT_LOG_DIR_ENV) {
            Ok(value) if !value.trim().is_empty() => util::resolve_path(base_dir, &value),
            _ => crate::runtime_paths::event_log_dir(base_dir),
        };
        let sqlite_path = match std::env::var(HARN_EVENT_LOG_SQLITE_PATH_ENV) {
            Ok(value) if !value.trim().is_empty() => util::resolve_path(base_dir, &value),
            _ => crate::runtime_paths::event_log_sqlite_path(base_dir),
        };

        Ok(Self {
            backend,
            file_dir,
            sqlite_path,
            queue_depth,
        })
    }

    pub fn location(&self) -> Option<PathBuf> {
        match self.backend {
            EventLogBackendKind::Memory => None,
            EventLogBackendKind::File => Some(self.file_dir.clone()),
            EventLogBackendKind::Sqlite => Some(self.sqlite_path.clone()),
        }
    }
}

thread_local! {
    static ACTIVE_EVENT_LOG: RefCell<Option<Arc<AnyEventLog>>> = const { RefCell::new(None) };
    static PENDING_DEFAULT_EVENT_LOG: RefCell<Option<EventLogConfig>> = const { RefCell::new(None) };
}

pub fn install_default_for_base_dir(base_dir: &Path) -> Result<Arc<AnyEventLog>, LogError> {
    let config = EventLogConfig::for_base_dir(base_dir)?;
    let log = open_event_log(&config)?;
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    PENDING_DEFAULT_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = None;
    });
    Ok(log)
}

pub fn install_lazy_default_for_base_dir(base_dir: &Path) -> Result<(), LogError> {
    let config = EventLogConfig::for_base_dir(base_dir)?;
    let has_active = ACTIVE_EVENT_LOG.with(|slot| slot.borrow().is_some());
    if !has_active {
        PENDING_DEFAULT_EVENT_LOG.with(|slot| {
            *slot.borrow_mut() = Some(config);
        });
    }
    Ok(())
}

pub fn install_memory_for_current_thread(queue_depth: usize) -> Arc<AnyEventLog> {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(queue_depth.max(1))));
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    PENDING_DEFAULT_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = None;
    });
    log
}

pub fn install_active_event_log(log: Arc<AnyEventLog>) -> Arc<AnyEventLog> {
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    PENDING_DEFAULT_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = None;
    });
    log
}

pub fn active_event_log() -> Option<Arc<AnyEventLog>> {
    if let Some(log) = ACTIVE_EVENT_LOG.with(|slot| slot.borrow().clone()) {
        return Some(log);
    }

    let config = PENDING_DEFAULT_EVENT_LOG.with(|slot| slot.borrow_mut().take())?;
    match open_event_log(&config) {
        Ok(log) => Some(install_active_event_log(log)),
        Err(error) => {
            crate::events::log_warn("event_log.init", &error.to_string());
            None
        }
    }
}

pub fn reset_active_event_log() {
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = None;
    });
    PENDING_DEFAULT_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

pub fn describe_for_base_dir(base_dir: &Path) -> Result<EventLogDescription, LogError> {
    let config = EventLogConfig::for_base_dir(base_dir)?;
    let description = match config.backend {
        EventLogBackendKind::Memory => EventLogDescription {
            backend: EventLogBackendKind::Memory,
            location: None,
            size_bytes: None,
            queue_depth: config.queue_depth,
        },
        EventLogBackendKind::File => EventLogDescription {
            backend: EventLogBackendKind::File,
            size_bytes: Some(util::dir_size_bytes(&config.file_dir)),
            location: Some(config.file_dir),
            queue_depth: config.queue_depth,
        },
        EventLogBackendKind::Sqlite => EventLogDescription {
            backend: EventLogBackendKind::Sqlite,
            size_bytes: Some(util::sqlite_size_bytes(&config.sqlite_path)),
            location: Some(config.sqlite_path),
            queue_depth: config.queue_depth,
        },
    };
    Ok(description)
}

pub fn open_event_log(config: &EventLogConfig) -> Result<Arc<AnyEventLog>, LogError> {
    match config.backend {
        EventLogBackendKind::Memory => Ok(Arc::new(AnyEventLog::Memory(MemoryEventLog::new(
            config.queue_depth,
        )))),
        EventLogBackendKind::File => Ok(Arc::new(AnyEventLog::File(FileEventLog::open(
            config.file_dir.clone(),
            config.queue_depth,
        )?))),
        EventLogBackendKind::Sqlite => Ok(Arc::new(AnyEventLog::Sqlite(SqliteEventLog::open(
            config.sqlite_path.clone(),
            config.queue_depth,
        )?))),
    }
}

pub enum AnyEventLog {
    Memory(MemoryEventLog),
    File(FileEventLog),
    Sqlite(SqliteEventLog),
}

impl AnyEventLog {
    pub async fn topics(&self) -> Result<Vec<Topic>, LogError> {
        match self {
            Self::Memory(log) => log.topics().await,
            Self::File(log) => log.topics(),
            Self::Sqlite(log) => log.topics(),
        }
    }

    pub async fn append_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        if header.trim().is_empty() {
            return Err(LogError::Config(
                "idempotent append header cannot be empty".to_string(),
            ));
        }
        match self {
            Self::Memory(log) => {
                log.append_idempotent_by_header(topic, header, value, event)
                    .await
            }
            Self::File(log) => log.append_idempotent_by_header(topic, header, value, event),
            Self::Sqlite(log) => log.append_idempotent_by_header(topic, header, value, event),
        }
    }

    /// Read the event previously appended under `(header, value)`, the read
    /// counterpart of [`Self::append_idempotent_by_header`]. On the SQLite
    /// backend this is an indexed JOIN; the memory/file dev backends scan.
    pub async fn read_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
    ) -> Result<Option<(EventId, LogEvent)>, LogError> {
        if header.trim().is_empty() {
            return Err(LogError::Config(
                "idempotent read header cannot be empty".to_string(),
            ));
        }
        match self {
            Self::Memory(log) => log.read_idempotent_by_header(topic, header, value).await,
            Self::File(log) => log.read_idempotent_by_header(topic, header, value),
            Self::Sqlite(log) => log.read_idempotent_by_header(topic, header, value),
        }
    }
}

impl EventLog for AnyEventLog {
    fn describe(&self) -> EventLogDescription {
        match self {
            Self::Memory(log) => log.describe(),
            Self::File(log) => log.describe(),
            Self::Sqlite(log) => log.describe(),
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        match self {
            Self::Memory(log) => log.append(topic, event).await,
            Self::File(log) => log.append(topic, event).await,
            Self::Sqlite(log) => log.append(topic, event).await,
        }
    }

    async fn flush(&self) -> Result<(), LogError> {
        match self {
            Self::Memory(log) => log.flush().await,
            Self::File(log) => log.flush().await,
            Self::Sqlite(log) => log.flush().await,
        }
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        match self {
            Self::Memory(log) => log.read_range(topic, from, limit).await,
            Self::File(log) => log.read_range(topic, from, limit).await,
            Self::Sqlite(log) => log.read_range(topic, from, limit).await,
        }
    }

    async fn read_range_bytes(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEventBytes)>, LogError> {
        match self {
            Self::Memory(log) => log.read_range_bytes(topic, from, limit).await,
            Self::File(log) => log.read_range_bytes(topic, from, limit).await,
            Self::Sqlite(log) => log.read_range_bytes(topic, from, limit).await,
        }
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let (rx, queue_depth) = match self.as_ref() {
            Self::Memory(log) => (
                log.broadcasts.subscribe(topic, log.queue_depth),
                log.queue_depth,
            ),
            Self::File(log) => (
                log.broadcasts.subscribe(topic, log.queue_depth),
                log.queue_depth,
            ),
            Self::Sqlite(log) => (
                log.broadcasts.subscribe(topic, log.queue_depth),
                log.queue_depth,
            ),
        };
        let history = self.read_range(topic, from, usize::MAX).await?;
        Ok(util::stream_from_broadcast(history, from, rx, queue_depth))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        match self {
            Self::Memory(log) => log.ack(topic, consumer, up_to).await,
            Self::File(log) => log.ack(topic, consumer, up_to).await,
            Self::Sqlite(log) => log.ack(topic, consumer, up_to).await,
        }
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        match self {
            Self::Memory(log) => log.consumer_cursor(topic, consumer).await,
            Self::File(log) => log.consumer_cursor(topic, consumer).await,
            Self::Sqlite(log) => log.consumer_cursor(topic, consumer).await,
        }
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        match self {
            Self::Memory(log) => log.latest(topic).await,
            Self::File(log) => log.latest(topic).await,
            Self::Sqlite(log) => log.latest(topic).await,
        }
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        match self {
            Self::Memory(log) => log.compact(topic, before).await,
            Self::File(log) => log.compact(topic, before).await,
            Self::Sqlite(log) => log.compact(topic, before).await,
        }
    }
}

pub fn sanitize_topic_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
