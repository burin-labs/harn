use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use super::util::{
    dir_size_bytes, now_ms, prepare_event_after, sanitize_filename, stream_from_broadcast,
    sync_tree, write_json_atomically, BroadcastMap,
};
use super::{
    AppendOutcome, CompactReport, ConsumerId, EventId, EventLog, EventLogBackendKind,
    EventLogDescription, LogError, LogEvent, LogEventBytes, Topic,
};

#[derive(Serialize, Deserialize)]
struct FileRecord {
    id: EventId,
    event: LogEvent,
}

pub struct FileEventLog {
    root: PathBuf,
    latest_ids: Mutex<HashMap<String, EventId>>,
    write_lock: Mutex<()>,
    pub(super) broadcasts: BroadcastMap,
    pub(super) queue_depth: usize,
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
            for record in read_file_records(&path)? {
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
        let from = from.unwrap_or(0);
        let mut events = Vec::new();
        for record in read_file_records(&path)? {
            if record.id > from {
                events.push((record.id, record.event));
            }
            if events.len() >= limit {
                break;
            }
        }
        Ok(events)
    }

    pub(super) fn topics(&self) -> Result<Vec<Topic>, LogError> {
        let topics_dir = self.root.join("topics");
        if !topics_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut topics = Vec::new();
        for entry in std::fs::read_dir(&topics_dir)
            .map_err(|error| LogError::Io(format!("event log topics read error: {error}")))?
        {
            let entry = entry
                .map_err(|error| LogError::Io(format!("event log topic entry error: {error}")))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            topics.push(Topic::new(stem.to_string())?);
        }
        topics.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(topics)
    }

    pub(super) fn append_idempotent_by_header(
        &self,
        topic: &Topic,
        header: &str,
        value: &str,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        let _guard = self
            .write_lock
            .lock()
            .expect("file event log write lock poisoned");
        let existing_events = self.read_range_sync(topic, None, usize::MAX)?;
        if let Some((event_id, existing)) = existing_events.iter().find(|(_, event)| {
            event
                .headers
                .get(header)
                .is_some_and(|found| found == value)
        }) {
            return Ok(AppendOutcome {
                event_id: *event_id,
                event: existing.clone(),
                inserted: false,
            });
        }

        let next_id = self.latest_id_for_topic(topic)? + 1;
        let previous = existing_events
            .last()
            .map(|(previous_id, previous_event)| (*previous_id, previous_event));
        let event = prepare_event_after(topic, next_id, previous, event)?;
        self.append_record_locked(topic, next_id, event)
    }

    fn append_record_locked(
        &self,
        topic: &Topic,
        event_id: EventId,
        event: LogEvent,
    ) -> Result<AppendOutcome, LogError> {
        let record = FileRecord {
            id: event_id,
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
            .insert(topic.as_str().to_string(), event_id);
        self.broadcasts
            .publish(topic, self.queue_depth, (event_id, event.clone()));
        Ok(AppendOutcome {
            event_id,
            event,
            inserted: true,
        })
    }
}

fn read_file_records(path: &Path) -> Result<Vec<FileRecord>, LogError> {
    let file = std::fs::File::open(path)
        .map_err(|error| LogError::Io(format!("event log open error: {error}")))?;
    let mut reader = std::io::BufReader::new(file);
    let mut records = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes_read = std::io::BufRead::read_until(&mut reader, b'\n', &mut line)
            .map_err(|error| LogError::Io(format!("event log read error: {error}")))?;
        if bytes_read == 0 {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let complete_line = line.ends_with(b"\n");
        match serde_json::from_slice::<FileRecord>(&line) {
            Ok(record) => records.push(record),
            Err(_) if !complete_line => break,
            Err(error) => {
                return Err(LogError::Serde(format!("event log parse error: {error}")));
            }
        }
    }
    Ok(records)
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
        let existing_events = self.read_range_sync(topic, None, usize::MAX)?;
        let previous = existing_events
            .last()
            .map(|(previous_id, previous_event)| (*previous_id, previous_event));
        let event = prepare_event_after(topic, next_id, previous, event)?;
        self.append_record_locked(topic, next_id, event)
            .map(|outcome| outcome.event_id)
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

    async fn read_range_bytes(
        &self,
        topic: &Topic,
        from: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, LogEventBytes)>, LogError> {
        self.read_range_sync(topic, from, limit)?
            .into_iter()
            .map(|(event_id, event)| Ok((event_id, event.try_into()?)))
            .collect()
    }

    async fn subscribe(
        self: Arc<Self>,
        topic: &Topic,
        from: Option<EventId>,
    ) -> Result<BoxStream<'static, Result<(EventId, LogEvent), LogError>>, LogError> {
        let rx = self.broadcasts.subscribe(topic, self.queue_depth);
        let history = self.read_range_sync(topic, from, usize::MAX)?;
        Ok(stream_from_broadcast(history, from, rx, self.queue_depth))
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
        if retained.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            crate::atomic_io::atomic_write_with(&path, |writer| {
                use std::io::Write as _;
                for (event_id, event) in &retained {
                    let line = serde_json::to_string(&FileRecord {
                        id: *event_id,
                        event: event.clone(),
                    })
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                    writeln!(writer, "{line}")?;
                }
                Ok(())
            })
            .map_err(|error| LogError::Io(format!("event log compact finalize error: {error}")))?;
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
