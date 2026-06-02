use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use futures::stream::BoxStream;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use super::{EventId, LogError, LogEvent, Topic};

#[derive(Default)]
pub(super) struct BroadcastMap(Mutex<HashMap<String, broadcast::Sender<(EventId, LogEvent)>>>);

impl BroadcastMap {
    pub(super) fn subscribe(
        &self,
        topic: &Topic,
        capacity: usize,
    ) -> broadcast::Receiver<(EventId, LogEvent)> {
        self.sender(topic, capacity).subscribe()
    }

    pub(super) fn publish(&self, topic: &Topic, capacity: usize, record: (EventId, LogEvent)) {
        let _ = self.sender(topic, capacity).send(record);
    }

    fn sender(&self, topic: &Topic, capacity: usize) -> broadcast::Sender<(EventId, LogEvent)> {
        let mut map = self.0.lock().expect("event log broadcast map poisoned");
        map.entry(topic.as_str().to_string())
            .or_insert_with(|| broadcast::channel(capacity.max(1)).0)
            .clone()
    }
}

pub(super) fn stream_from_broadcast(
    history: Vec<(EventId, LogEvent)>,
    from: Option<EventId>,
    mut live_rx: broadcast::Receiver<(EventId, LogEvent)>,
    queue_depth: usize,
) -> BoxStream<'static, Result<(EventId, LogEvent), LogError>> {
    let (tx, rx) = mpsc::channel(queue_depth.max(1));
    // Run the subscription forwarder as a tokio task rather than a detached
    // OS thread. A dedicated thread running under `futures::executor::block_on`
    // is invisible to the tokio runtime, so tests that use `start_paused = true`
    // race against auto-advanced timers while the thread catches up in real
    // time. Spawning on tokio makes the forwarder participate in runtime
    // scheduling (including paused-time quiescence) and ties its lifetime to
    // the runtime's shutdown.
    tokio::spawn(async move {
        let mut last_seen = from.unwrap_or(0);
        for (event_id, event) in history {
            last_seen = event_id;
            if tx.send(Ok((event_id, event))).await.is_err() {
                return;
            }
        }

        loop {
            tokio::select! {
                _ = tx.closed() => return,
                received = live_rx.recv() => {
                    match received {
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
            }
        }
    });
    Box::pin(ReceiverStream::new(rx))
}

pub(super) fn prepare_event_after(
    topic: &Topic,
    event_id: EventId,
    previous: Option<(EventId, &LogEvent)>,
    event: LogEvent,
) -> Result<LogEvent, LogError> {
    let previous_hash = previous
        .map(|(previous_id, previous_event)| {
            crate::provenance::event_record_hash_from_headers(
                topic.as_str(),
                previous_id,
                previous_event,
            )
        })
        .transpose()?;
    crate::provenance::prepare_event_for_append(topic.as_str(), event_id, previous_hash, event)
}

pub(super) fn resolve_path(base_dir: &Path, value: &str) -> PathBuf {
    let candidate = PathBuf::from(value);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

pub(super) fn write_json_atomically(
    path: &Path,
    payload: &serde_json::Value,
) -> Result<(), LogError> {
    let encoded = serde_json::to_vec_pretty(payload)
        .map_err(|error| LogError::Serde(format!("event log encode error: {error}")))?;
    crate::atomic_io::atomic_write(path, &encoded)
        .map_err(|error| LogError::Io(format!("event log write error: {error}")))
}

pub(super) fn sanitize_filename(value: &str) -> String {
    super::sanitize_topic_component(value)
}

pub(super) fn dir_size_bytes(path: &Path) -> u64 {
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

pub(super) fn sqlite_size_bytes(path: &Path) -> u64 {
    let mut total = file_size(path);
    total += file_size(&PathBuf::from(format!("{}-wal", path.display())));
    total += file_size(&PathBuf::from(format!("{}-shm", path.display())));
    total
}

pub(super) fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

pub(super) fn sync_tree(root: &Path) -> Result<(), LogError> {
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
        fsync_file(&path)
            .map_err(|error| LogError::Io(format!("event log sync error: {error}")))?;
    }
    Ok(())
}

/// Flush a file's contents to durable storage in a cross-platform way.
///
/// `File::sync_all` lowers to `FlushFileBuffers` on Windows, which requires a
/// handle opened with **write** access — calling it on a read-only handle
/// (`File::open`) fails with `ERROR_ACCESS_DENIED` ("Access is denied"). On
/// Unix, `fsync(2)` on a read-only descriptor is fine, which is why opening
/// read-only "worked" everywhere except Windows. Always open for write so the
/// sync succeeds on every platform.
///
/// If the file genuinely can't be opened for writing (e.g. read-only
/// permissions), fall back to a read-only sync. That still flushes on Unix,
/// and on Windows the OS write-back cache will persist the data on its own —
/// so a durability *flush* must never become a hard error just because a file
/// isn't writable.
fn fsync_file(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file.sync_all(),
        Err(write_err) => match std::fs::File::open(path) {
            // Best-effort on a read-only file: succeeds on Unix; on Windows the
            // access-denied here is expected and not worth failing the flush.
            Ok(file) => file.sync_all().or(Ok(())),
            // Surface the original write-open failure (e.g. the file vanished).
            Err(_) => Err(write_err),
        },
    }
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn event_id_to_sqlite_i64(event_id: EventId) -> Result<i64, LogError> {
    i64::try_from(event_id)
        .map_err(|_| LogError::Sqlite(format!("event id {event_id} exceeds sqlite INTEGER range")))
}

pub(super) fn sqlite_i64_to_event_id(value: i64) -> Result<EventId, LogError> {
    u64::try_from(value)
        .map_err(|_| LogError::Sqlite(format!("sqlite event id {value} is negative")))
}

pub(super) fn sqlite_i64_to_event_id_for_row(value: i64) -> rusqlite::Result<EventId> {
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            std::mem::size_of::<i64>(),
            rusqlite::types::Type::Integer,
            "sqlite event id is negative".into(),
        )
    })
}

pub(super) fn sqlite_json_bytes_for_row(
    row: &rusqlite::Row<'_>,
    index: usize,
    name: &str,
) -> rusqlite::Result<Vec<u8>> {
    let value = row.get_ref(index)?;
    match value {
        rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
            Ok(bytes.to_vec())
        }
        other => Err(rusqlite::Error::InvalidColumnType(
            index,
            name.to_string(),
            other.data_type(),
        )),
    }
}

pub(super) fn sqlite_i64_to_usize(value: i64) -> Result<usize, LogError> {
    usize::try_from(value)
        .map_err(|_| LogError::Sqlite(format!("sqlite count {value} is negative")))
}
