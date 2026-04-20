use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
            occurred_at_ms: now_ms(),
        }
    }

    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.headers = headers;
        self
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
            .unwrap_or(128)
            .max(1);

        let file_dir = match std::env::var(HARN_EVENT_LOG_DIR_ENV) {
            Ok(value) if !value.trim().is_empty() => resolve_path(base_dir, &value),
            _ => crate::runtime_paths::event_log_dir(base_dir),
        };
        let sqlite_path = match std::env::var(HARN_EVENT_LOG_SQLITE_PATH_ENV) {
            Ok(value) if !value.trim().is_empty() => resolve_path(base_dir, &value),
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
}

pub fn install_default_for_base_dir(base_dir: &Path) -> Result<Arc<AnyEventLog>, LogError> {
    let config = EventLogConfig::for_base_dir(base_dir)?;
    let log = open_event_log(&config)?;
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    Ok(log)
}

pub fn install_memory_for_current_thread(queue_depth: usize) -> Arc<AnyEventLog> {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(queue_depth.max(1))));
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    log
}

pub fn install_active_event_log(log: Arc<AnyEventLog>) -> Arc<AnyEventLog> {
    ACTIVE_EVENT_LOG.with(|slot| {
        *slot.borrow_mut() = Some(log.clone());
    });
    log
}

pub fn active_event_log() -> Option<Arc<AnyEventLog>> {
    ACTIVE_EVENT_LOG.with(|slot| slot.borrow().clone())
}

pub fn reset_active_event_log() {
    ACTIVE_EVENT_LOG.with(|slot| {
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
            size_bytes: Some(dir_size_bytes(&config.file_dir)),
            location: Some(config.file_dir),
            queue_depth: config.queue_depth,
        },
        EventLogBackendKind::Sqlite => EventLogDescription {
            backend: EventLogBackendKind::Sqlite,
            size_bytes: Some(sqlite_size_bytes(&config.sqlite_path)),
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
        Ok(stream_from_broadcast(
            self,
            topic.clone(),
            from,
            rx,
            queue_depth,
        ))
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

#[derive(Default)]
struct BroadcastMap(Mutex<HashMap<String, broadcast::Sender<(EventId, LogEvent)>>>);

impl BroadcastMap {
    fn subscribe(
        &self,
        topic: &Topic,
        capacity: usize,
    ) -> broadcast::Receiver<(EventId, LogEvent)> {
        self.sender(topic, capacity).subscribe()
    }

    fn publish(&self, topic: &Topic, capacity: usize, record: (EventId, LogEvent)) {
        let _ = self.sender(topic, capacity).send(record);
    }

    fn sender(&self, topic: &Topic, capacity: usize) -> broadcast::Sender<(EventId, LogEvent)> {
        let mut map = self.0.lock().expect("event log broadcast map poisoned");
        map.entry(topic.as_str().to_string())
            .or_insert_with(|| broadcast::channel(capacity.max(1)).0)
            .clone()
    }
}

fn stream_from_broadcast<L>(
    log: Arc<L>,
    topic: Topic,
    from: Option<EventId>,
    mut live_rx: broadcast::Receiver<(EventId, LogEvent)>,
    queue_depth: usize,
) -> BoxStream<'static, Result<(EventId, LogEvent), LogError>>
where
    L: EventLog + 'static,
{
    let (tx, rx) = mpsc::channel(queue_depth.max(1));
    std::thread::spawn(move || {
        futures::executor::block_on(async move {
            let history = match log.read_range(&topic, from, usize::MAX).await {
                Ok(history) => history,
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };

            let mut last_seen = from.unwrap_or(0);
            for (event_id, event) in history {
                last_seen = event_id;
                if tx.send(Ok((event_id, event))).await.is_err() {
                    return;
                }
            }

            loop {
                match live_rx.recv().await {
                    Ok((event_id, event)) if event_id > last_seen => {
                        last_seen = event_id;
                        if tx.send(Ok((event_id, event))).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = tx.try_send(Err(LogError::ConsumerLagged(last_seen)));
                        return;
                    }
                }
            }
        });
    });
    Box::pin(ReceiverStream::new(rx))
}

#[derive(Default)]
struct MemoryState {
    topics: HashMap<String, VecDeque<(EventId, LogEvent)>>,
    latest: HashMap<String, EventId>,
    consumers: HashMap<(String, String), EventId>,
}

pub struct MemoryEventLog {
    state: tokio::sync::Mutex<MemoryState>,
    broadcasts: BroadcastMap,
    queue_depth: usize,
}

impl MemoryEventLog {
    pub fn new(queue_depth: usize) -> Self {
        Self {
            state: tokio::sync::Mutex::new(MemoryState::default()),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        }
    }
}

impl EventLog for MemoryEventLog {
    fn describe(&self) -> EventLogDescription {
        EventLogDescription {
            backend: EventLogBackendKind::Memory,
            location: None,
            size_bytes: None,
            queue_depth: self.queue_depth,
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        let mut state = self.state.lock().await;
        let event_id = state.latest.get(topic.as_str()).copied().unwrap_or(0) + 1;
        state.latest.insert(topic.as_str().to_string(), event_id);
        state
            .topics
            .entry(topic.as_str().to_string())
            .or_default()
            .push_back((event_id, event.clone()));
        drop(state);
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event));
        Ok(event_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        Ok(())
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        let from = from.unwrap_or(0);
        let state = self.state.lock().await;
        let events = state
            .topics
            .get(topic.as_str())
            .into_iter()
            .flat_map(|events| events.iter())
            .filter(|(event_id, _)| *event_id > from)
            .take(limit)
            .map(|(event_id, event)| (*event_id, event.clone()))
            .collect();
        Ok(events)
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        Ok(stream_from_broadcast(
            self.clone(),
            topic.clone(),
            from,
            rx,
            self.queue_depth,
        ))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        let mut state = self.state.lock().await;
        state.consumers.insert(
            (topic.as_str().to_string(), consumer.as_str().to_string()),
            up_to,
        );
        Ok(())
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let state = self.state.lock().await;
        Ok(state
            .consumers
            .get(&(topic.as_str().to_string(), consumer.as_str().to_string()))
            .copied())
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let state = self.state.lock().await;
        Ok(state.latest.get(topic.as_str()).copied())
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let mut state = self.state.lock().await;
        let Some(events) = state.topics.get_mut(topic.as_str()) else {
            return Ok(CompactReport::default());
        };
        let removed = events
            .iter()
            .take_while(|(event_id, _)| *event_id <= before)
            .count();
        for _ in 0..removed {
            events.pop_front();
        }
        Ok(CompactReport {
            removed,
            remaining: events.len(),
            latest: state.latest.get(topic.as_str()).copied(),
            checkpointed: false,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct FileRecord {
    id: EventId,
    event: LogEvent,
}

pub struct FileEventLog {
    root: PathBuf,
    latest_ids: Mutex<HashMap<String, EventId>>,
    write_lock: Mutex<()>,
    broadcasts: BroadcastMap,
    queue_depth: usize,
}

impl FileEventLog {
    pub fn open(root: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        std::fs::create_dir_all(root.join("topics"))
            .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        std::fs::create_dir_all(root.join("consumers"))
            .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        Ok(Self {
            root,
            latest_ids: Mutex::new(HashMap::new()),
            write_lock: Mutex::new(()),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }

    fn topic_path(&self, topic: &Topic) -> PathBuf {
        self.root
            .join("topics")
            .join(format!("{}.jsonl", topic.as_str()))
    }

    fn consumer_path(&self, topic: &Topic, consumer: &ConsumerId) -> PathBuf {
        self.root.join("consumers").join(format!(
            "{}__{}.json",
            topic.as_str(),
            sanitize_filename(consumer.as_str())
        ))
    }

    fn latest_id_for_topic(&self, topic: &Topic) -> Result<EventId, LogError> {
        if let Some(event_id) = self
            .latest_ids
            .lock()
            .expect("file event log latest ids poisoned")
            .get(topic.as_str())
            .copied()
        {
            return Ok(event_id);
        }

        let mut latest = 0;
        let path = self.topic_path(topic);
        if path.is_file() {
            let file = std::fs::File::open(&path)
                .map_err(|error| LogError::Io(format!("event log open error: {error}")))?;
            for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
                let line =
                    line.map_err(|error| LogError::Io(format!("event log read error: {error}")))?;
                let record: FileRecord = serde_json::from_str(&line)
                    .map_err(|error| LogError::Serde(format!("event log parse error: {error}")))?;
                latest = record.id;
            }
        }
        self.latest_ids
            .lock()
            .expect("file event log latest ids poisoned")
            .insert(topic.as_str().to_string(), latest);
        Ok(latest)
    }

    fn read_range_sync(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        let path = self.topic_path(topic);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&path)
            .map_err(|error| LogError::Io(format!("event log open error: {error}")))?;
        let from = from.unwrap_or(0);
        let mut events = Vec::new();
        for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
            let line =
                line.map_err(|error| LogError::Io(format!("event log read error: {error}")))?;
            let record: FileRecord = serde_json::from_str(&line)
                .map_err(|error| LogError::Serde(format!("event log parse error: {error}")))?;
            if record.id > from {
                events.push((record.id, record.event));
            }
            if events.len() >= limit {
                break;
            }
        }
        Ok(events)
    }
}

impl EventLog for FileEventLog {
    fn describe(&self) -> EventLogDescription {
        EventLogDescription {
            backend: EventLogBackendKind::File,
            location: Some(self.root.clone()),
            size_bytes: Some(dir_size_bytes(&self.root)),
            queue_depth: self.queue_depth,
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        let _guard = self
            .write_lock
            .lock()
            .expect("file event log write lock poisoned");
        let next_id = self.latest_id_for_topic(topic)? + 1;
        let record = FileRecord {
            id: next_id,
            event: event.clone(),
        };
        let path = self.topic_path(topic);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        }
        let line = serde_json::to_string(&record)
            .map_err(|error| LogError::Serde(format!("event log encode error: {error}")))?;
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| LogError::Io(format!("event log open error: {error}")))?;
        writeln!(file, "{line}")
            .map_err(|error| LogError::Io(format!("event log write error: {error}")))?;
        self.latest_ids
            .lock()
            .expect("file event log latest ids poisoned")
            .insert(topic.as_str().to_string(), next_id);
        self.broadcasts
            .publish(topic, self.queue_depth, (next_id, event));
        Ok(next_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        sync_tree(&self.root)
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        self.read_range_sync(topic, from, limit)
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        Ok(stream_from_broadcast(
            self.clone(),
            topic.clone(),
            from,
            rx,
            self.queue_depth,
        ))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        let path = self.consumer_path(topic, consumer);
        let payload = serde_json::json!({
            "topic": topic.as_str(),
            "consumer_id": consumer.as_str(),
            "cursor": up_to,
            "updated_at_ms": now_ms(),
        });
        write_json_atomically(&path, &payload)
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let path = self.consumer_path(topic, consumer);
        if !path.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| LogError::Io(format!("event log consumer read error: {error}")))?;
        let payload: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| LogError::Serde(format!("event log consumer parse error: {error}")))?;
        let cursor = payload
            .get("cursor")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                LogError::Serde("event log consumer record missing numeric cursor".to_string())
            })?;
        Ok(Some(cursor))
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let latest = self.latest_id_for_topic(topic)?;
        if latest == 0 {
            Ok(None)
        } else {
            Ok(Some(latest))
        }
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let _guard = self
            .write_lock
            .lock()
            .expect("file event log write lock poisoned");
        let path = self.topic_path(topic);
        if !path.is_file() {
            return Ok(CompactReport::default());
        }
        let retained = self.read_range_sync(topic, Some(before), usize::MAX)?;
        let removed = self.read_range_sync(topic, None, usize::MAX)?.len() - retained.len();
        let tmp = path.with_extension("jsonl.tmp");
        if retained.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            let mut writer =
                std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|error| {
                    LogError::Io(format!("event log tmp create error: {error}"))
                })?);
            use std::io::Write as _;
            for (event_id, event) in &retained {
                let line = serde_json::to_string(&FileRecord {
                    id: *event_id,
                    event: event.clone(),
                })
                .map_err(|error| LogError::Serde(format!("event log encode error: {error}")))?;
                writeln!(writer, "{line}")
                    .map_err(|error| LogError::Io(format!("event log write error: {error}")))?;
            }
            writer
                .flush()
                .map_err(|error| LogError::Io(format!("event log flush error: {error}")))?;
            std::fs::rename(&tmp, &path).map_err(|error| {
                LogError::Io(format!("event log compact finalize error: {error}"))
            })?;
        }
        let latest = retained.last().map(|(event_id, _)| *event_id);
        self.latest_ids
            .lock()
            .expect("file event log latest ids poisoned")
            .insert(topic.as_str().to_string(), latest.unwrap_or(0));
        Ok(CompactReport {
            removed,
            remaining: retained.len(),
            latest,
            checkpointed: false,
        })
    }
}

pub struct SqliteEventLog {
    path: PathBuf,
    connection: Mutex<Connection>,
    broadcasts: BroadcastMap,
    queue_depth: usize,
}

impl SqliteEventLog {
    pub fn open(path: PathBuf, queue_depth: usize) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
        }
        let connection = Connection::open(&path)
            .map_err(|error| LogError::Sqlite(format!("event log open error: {error}")))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| LogError::Sqlite(format!("event log WAL pragma error: {error}")))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| LogError::Sqlite(format!("event log sync pragma error: {error}")))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| LogError::Sqlite(format!("event log busy-timeout error: {error}")))?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS topic_heads (
                    topic TEXT PRIMARY KEY,
                    last_id INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS events (
                    topic TEXT NOT NULL,
                    event_id INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    headers TEXT NOT NULL,
                    occurred_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (topic, event_id)
                );
                CREATE TABLE IF NOT EXISTS consumers (
                    topic TEXT NOT NULL,
                    consumer_id TEXT NOT NULL,
                    cursor INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (topic, consumer_id)
                );",
            )
            .map_err(|error| LogError::Sqlite(format!("event log schema error: {error}")))?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            broadcasts: BroadcastMap::default(),
            queue_depth: queue_depth.max(1),
        })
    }
}

impl EventLog for SqliteEventLog {
    fn describe(&self) -> EventLogDescription {
        EventLogDescription {
            backend: EventLogBackendKind::Sqlite,
            location: Some(self.path.clone()),
            size_bytes: Some(sqlite_size_bytes(&self.path)),
            queue_depth: self.queue_depth,
        }
    }

    async fn append(&self, topic: &Topic, event: LogEvent) -> Result<EventId, LogError> {
        let mut connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| LogError::Sqlite(format!("event log transaction error: {error}")))?;
        tx.execute(
            "INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES (?1, 0)",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head init error: {error}")))?;
        tx.execute(
            "UPDATE topic_heads SET last_id = last_id + 1 WHERE topic = ?1",
            params![topic.as_str()],
        )
        .map_err(|error| LogError::Sqlite(format!("event log head update error: {error}")))?;
        let event_id: EventId = tx
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| LogError::Sqlite(format!("event log head read error: {error}")))?;
        tx.execute(
            "INSERT INTO events(topic, event_id, kind, payload, headers, occurred_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                topic.as_str(),
                event_id,
                event.kind,
                serde_json::to_string(&event.payload).map_err(|error| LogError::Serde(format!(
                    "event log payload encode error: {error}"
                )))?,
                serde_json::to_string(&event.headers).map_err(|error| LogError::Serde(format!(
                    "event log headers encode error: {error}"
                )))?,
                event.occurred_at_ms
            ],
        )
        .map_err(|error| LogError::Sqlite(format!("event log insert error: {error}")))?;
        tx.commit()
            .map_err(|error| LogError::Sqlite(format!("event log commit error: {error}")))?;
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event.clone()));
        Ok(event_id)
    }

    async fn flush(&self) -> Result<(), LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .map_err(|error| LogError::Sqlite(format!("event log checkpoint error: {error}")))?;
        Ok(())
    }

    async fn read_range(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEvent)>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT event_id, kind, payload, headers, occurred_at_ms
                 FROM events
                 WHERE topic = ?1 AND event_id > ?2
                 ORDER BY event_id ASC
                 LIMIT ?3",
            )
            .map_err(|error| LogError::Sqlite(format!("event log prepare error: {error}")))?;
        let rows = statement
            .query_map(
                params![topic.as_str(), from.unwrap_or(0), limit as i64],
                |row| {
                    let payload: String = row.get(2)?;
                    let headers: String = row.get(3)?;
                    Ok((
                        row.get::<_, EventId>(0)?,
                        LogEvent {
                            kind: row.get(1)?,
                            payload: serde_json::from_str(&payload).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    payload.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            headers: serde_json::from_str(&headers).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    headers.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                            occurred_at_ms: row.get(4)?,
                        },
                    ))
                },
            )
            .map_err(|error| LogError::Sqlite(format!("event log query error: {error}")))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(
                row.map_err(|error| LogError::Sqlite(format!("event log row error: {error}")))?,
            );
        }
        Ok(events)
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        Ok(stream_from_broadcast(
            self.clone(),
            topic.clone(),
            from,
            rx,
            self.queue_depth,
        ))
    }

    async fn ack(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
        up_to: EventId,
    ) -> Result<(), LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .execute(
                "INSERT INTO consumers(topic, consumer_id, cursor, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(topic, consumer_id)
                 DO UPDATE SET cursor = excluded.cursor, updated_at_ms = excluded.updated_at_ms",
                params![topic.as_str(), consumer.as_str(), up_to, now_ms()],
            )
            .map_err(|error| LogError::Sqlite(format!("event log ack error: {error}")))?;
        Ok(())
    }

    async fn consumer_cursor(
        &self,
        topic: &Topic,
        consumer: &ConsumerId,
    ) -> Result<Option<EventId>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .query_row(
                "SELECT cursor FROM consumers WHERE topic = ?1 AND consumer_id = ?2",
                params![topic.as_str(), consumer.as_str()],
                |row| row.get::<_, EventId>(0),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log consumer cursor error: {error}")))
    }

    async fn latest(&self, topic: &Topic) -> Result<Option<EventId>, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, EventId>(0),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log latest error: {error}")))
    }

    async fn compact(&self, topic: &Topic, before: EventId) -> Result<CompactReport, LogError> {
        let connection = self
            .connection
            .lock()
            .expect("sqlite event log connection poisoned");
        let removed = connection
            .execute(
                "DELETE FROM events WHERE topic = ?1 AND event_id <= ?2",
                params![topic.as_str(), before],
            )
            .map_err(|error| {
                LogError::Sqlite(format!("event log compact delete error: {error}"))
            })?;
        let remaining: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| LogError::Sqlite(format!("event log compact count error: {error}")))?;
        let latest = connection
            .query_row(
                "SELECT last_id FROM topic_heads WHERE topic = ?1",
                params![topic.as_str()],
                |row| row.get::<_, EventId>(0),
            )
            .optional()
            .map_err(|error| LogError::Sqlite(format!("event log latest error: {error}")))?;
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| LogError::Sqlite(format!("event log checkpoint error: {error}")))?;
        Ok(CompactReport {
            removed,
            remaining,
            latest,
            checkpointed: true,
        })
    }
}

fn resolve_path(base_dir: &Path, value: &str) -> PathBuf {
    let candidate = PathBuf::from(value);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

fn write_json_atomically(path: &Path, payload: &serde_json::Value) -> Result<(), LogError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| LogError::Io(format!("event log mkdir error: {error}")))?;
    }
    let tmp = path.with_extension("tmp");
    let encoded = serde_json::to_vec_pretty(payload)
        .map_err(|error| LogError::Serde(format!("event log encode error: {error}")))?;
    std::fs::write(&tmp, encoded)
        .map_err(|error| LogError::Io(format!("event log write error: {error}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|error| LogError::Io(format!("event log rename error: {error}")))?;
    Ok(())
}

fn sanitize_filename(value: &str) -> String {
    sanitize_topic_component(value)
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

fn dir_size_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size_bytes(&path);
            } else if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

fn sqlite_size_bytes(path: &Path) -> u64 {
    let mut total = file_size(path);
    total += file_size(&PathBuf::from(format!("{}-wal", path.display())));
    total += file_size(&PathBuf::from(format!("{}-shm", path.display())));
    total
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn sync_tree(root: &Path) -> Result<(), LogError> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)
        .map_err(|error| LogError::Io(format!("event log read_dir error: {error}")))?
    {
        let entry = entry.map_err(|error| LogError::Io(format!("event log dir error: {error}")))?;
        let path = entry.path();
        if path.is_dir() {
            sync_tree(&path)?;
            continue;
        }
        std::fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|error| LogError::Io(format!("event log sync error: {error}")))?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use rand::{rngs::StdRng, RngExt, SeedableRng};

    async fn exercise_basic_backend(log: Arc<AnyEventLog>) {
        let topic = Topic::new("trigger.inbox").unwrap();
        for i in 0..10_000 {
            log.append(
                &topic,
                LogEvent::new("append", serde_json::json!({ "i": i })),
            )
            .await
            .unwrap();
        }
        let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
        assert_eq!(events.len(), 10_000);
        assert_eq!(events.first().unwrap().0, 1);
        assert_eq!(events.last().unwrap().0, 10_000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn memory_backend_supports_append_read_subscribe_and_compact() {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
        exercise_basic_backend(log.clone()).await;

        let topic = Topic::new("agent.transcript.demo").unwrap();
        let mut stream = log.clone().subscribe(&topic, None).await.unwrap();
        let first = log
            .append(
                &topic,
                LogEvent::new("message", serde_json::json!({"text":"one"})),
            )
            .await
            .unwrap();
        let second = log
            .append(
                &topic,
                LogEvent::new("message", serde_json::json!({"text":"two"})),
            )
            .await
            .unwrap();
        let seen: Vec<_> = stream.by_ref().take(2).collect().await;
        assert_eq!(seen[0].as_ref().unwrap().0, first);
        assert_eq!(seen[1].as_ref().unwrap().0, second);

        log.ack(&topic, &ConsumerId::new("worker").unwrap(), second)
            .await
            .unwrap();
        let compact = log.compact(&topic, first).await.unwrap();
        assert_eq!(compact.removed, 1);
        assert_eq!(compact.remaining, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_backend_persists_across_reopen_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let topic = Topic::new("trigger.outbox").unwrap();
        let first_log = Arc::new(AnyEventLog::File(
            FileEventLog::open(dir.path().to_path_buf(), 8).unwrap(),
        ));
        first_log
            .append(
                &topic,
                LogEvent::new("dispatch_pending", serde_json::json!({"n":1})),
            )
            .await
            .unwrap();
        first_log
            .append(
                &topic,
                LogEvent::new("dispatch_complete", serde_json::json!({"n":2})),
            )
            .await
            .unwrap();
        drop(first_log);

        let reopened = Arc::new(AnyEventLog::File(
            FileEventLog::open(dir.path().to_path_buf(), 8).unwrap(),
        ));
        let events = reopened.read_range(&topic, None, usize::MAX).await.unwrap();
        assert_eq!(events.len(), 2);
        let compact = reopened.compact(&topic, 1).await.unwrap();
        assert_eq!(compact.removed, 1);
        assert_eq!(
            reopened
                .read_range(&topic, None, usize::MAX)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sqlite_backend_persists_and_checkpoints_after_compact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let topic = Topic::new("daemon.demo.state").unwrap();
        let first_log = Arc::new(AnyEventLog::Sqlite(
            SqliteEventLog::open(path.clone(), 8).unwrap(),
        ));
        first_log
            .append(
                &topic,
                LogEvent::new("state", serde_json::json!({"state":"idle"})),
            )
            .await
            .unwrap();
        first_log
            .append(
                &topic,
                LogEvent::new("state", serde_json::json!({"state":"active"})),
            )
            .await
            .unwrap();
        drop(first_log);

        let reopened = Arc::new(AnyEventLog::Sqlite(
            SqliteEventLog::open(path.clone(), 8).unwrap(),
        ));
        assert_eq!(
            reopened
                .read_range(&topic, None, usize::MAX)
                .await
                .unwrap()
                .len(),
            2
        );
        let compact = reopened.compact(&topic, 1).await.unwrap();
        assert!(compact.checkpointed);
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        assert!(file_size(&wal) == 0 || !wal.exists());
    }

    // CI-flaky: the broadcast buffer's lag detection is timing-dependent and
    // passes reliably under local single-threaded runtimes but occasionally
    // observes all 10 ticks before the lag signal surfaces under Linux CI
    // runners. Ignored on CI; still runs under `cargo test -- --ignored`.
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn subscriber_reports_lag_when_broadcast_buffer_overflows() {
        let log = Arc::new(MemoryEventLog::new(2));
        let topic = Topic::new("lag.demo").unwrap();
        let mut stream = log.clone().subscribe(&topic, None).await.unwrap();
        for i in 0..10 {
            log.append(&topic, LogEvent::new("tick", serde_json::json!({"i": i})))
                .await
                .unwrap();
        }
        let mut saw_lag = false;
        for _ in 0..4 {
            match stream.next().await {
                Some(Err(LogError::ConsumerLagged(_))) => {
                    saw_lag = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_lag, "subscriber should surface lag");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn randomized_reader_sequences_stay_monotonic() {
        let log = Arc::new(MemoryEventLog::new(32));
        let topic = Topic::new("fuzz.demo").unwrap();
        let mut readers = vec![
            log.clone().subscribe(&topic, None).await.unwrap(),
            log.clone().subscribe(&topic, Some(5)).await.unwrap(),
            log.clone().subscribe(&topic, Some(10)).await.unwrap(),
        ];
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..64 {
            let value = rng.random_range(0..1000);
            log.append(
                &topic,
                LogEvent::new("rand", serde_json::json!({"value": value})),
            )
            .await
            .unwrap();
        }

        let mut sequences = Vec::new();
        for reader in &mut readers {
            let mut ids = Vec::new();
            while let Some(item) = reader.next().await {
                match item {
                    Ok((event_id, _)) => {
                        ids.push(event_id);
                        if ids.len() >= 16 {
                            break;
                        }
                    }
                    Err(LogError::ConsumerLagged(_)) => break,
                    Err(error) => panic!("unexpected subscription error: {error}"),
                }
            }
            sequences.push(ids);
        }

        for ids in sequences {
            assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
