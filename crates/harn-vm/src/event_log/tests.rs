use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rand::{rngs::StdRng, RngExt, SeedableRng};
use rusqlite::{params, Connection};
use tokio::sync::broadcast;

use super::util::{file_size, stream_from_broadcast};
use super::{
    active_event_log, install_lazy_default_for_base_dir, reset_active_event_log, AnyEventLog,
    ConsumerId, EventLog, EventLogBackendKind, EventLogConfig, FileEventLog, LogError, LogEvent,
    MemoryEventLog, SqliteEventLog, Topic, ACTIVE_EVENT_LOG, PENDING_DEFAULT_EVENT_LOG,
};

#[test]
fn postgres_backend_kind_round_trips_as_metadata() {
    assert_eq!(
        "postgres".parse::<EventLogBackendKind>().unwrap(),
        EventLogBackendKind::Postgres
    );
    assert_eq!(EventLogBackendKind::Postgres.to_string(), "postgres");
    assert_eq!(
        serde_json::to_value(EventLogBackendKind::Postgres).unwrap(),
        serde_json::json!("postgres")
    );
    assert_eq!(
        serde_json::from_value::<EventLogBackendKind>(serde_json::json!("postgres")).unwrap(),
        EventLogBackendKind::Postgres
    );
}

#[test]
fn built_in_factory_rejects_host_provided_postgres_backend() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = EventLogConfig::for_base_dir(dir.path()).unwrap();
    config.backend = EventLogBackendKind::Postgres;

    let error = match super::open_event_log(&config) {
        Ok(_) => panic!("postgres is host-provided"),
        Err(error) => error,
    };
    assert!(matches!(error, LogError::Config(message) if message.contains("host-provided")));
}

#[test]
fn lazy_default_event_log_opens_on_first_access() {
    reset_active_event_log();
    let dir = tempfile::tempdir().unwrap();

    install_lazy_default_for_base_dir(dir.path()).unwrap();
    assert!(ACTIVE_EVENT_LOG.with(|slot| slot.borrow().is_none()));
    assert!(PENDING_DEFAULT_EVENT_LOG.with(|slot| slot.borrow().is_some()));

    let _log = active_event_log().expect("lazy event log should open on demand");
    assert!(ACTIVE_EVENT_LOG.with(|slot| slot.borrow().is_some()));
    assert!(PENDING_DEFAULT_EVENT_LOG.with(|slot| slot.borrow().is_none()));
    reset_active_event_log();
}

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

async fn exercise_idempotent_append(log: Arc<AnyEventLog>) {
    let topic = Topic::new("channel.tenant.default.pr").unwrap();
    let mut first_headers = BTreeMap::new();
    first_headers.insert("harn.channel.id".to_string(), "event-1".to_string());
    let first = log
        .append_idempotent_by_header(
            &topic,
            "harn.channel.id",
            "event-1",
            LogEvent::new("channel.emit", serde_json::json!({"n": 1})).with_headers(first_headers),
        )
        .await
        .unwrap();
    assert!(first.inserted);
    assert_eq!(first.event_id, 1);

    let mut duplicate_headers = BTreeMap::new();
    duplicate_headers.insert("harn.channel.id".to_string(), "event-1".to_string());
    let duplicate = log
        .append_idempotent_by_header(
            &topic,
            "harn.channel.id",
            "event-1",
            LogEvent::new("channel.emit", serde_json::json!({"n": 2}))
                .with_headers(duplicate_headers),
        )
        .await
        .unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(duplicate.event_id, first.event_id);
    assert_eq!(duplicate.event.payload, serde_json::json!({"n": 1}));

    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, first.event_id);

    // The indexed read counterpart resolves the same event by header...
    let found = log
        .read_idempotent_by_header(&topic, "harn.channel.id", "event-1")
        .await
        .unwrap();
    let (found_id, found_event) = found.expect("idempotent read should find the appended event");
    assert_eq!(found_id, first.event_id);
    assert_eq!(found_event.payload, serde_json::json!({"n": 1}));
    // ...and reports a miss for an unknown value rather than scanning forever.
    let missing = log
        .read_idempotent_by_header(&topic, "harn.channel.id", "event-404")
        .await
        .unwrap();
    assert!(missing.is_none());
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
async fn memory_backend_idempotent_append_returns_original_event() {
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(8)));
    exercise_idempotent_append(log).await;
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
async fn file_backend_skips_torn_tail_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let topic = Topic::new("trigger.inbox").unwrap();
    let first_log = FileEventLog::open(dir.path().to_path_buf(), 8).unwrap();
    first_log
        .append(
            &topic,
            LogEvent::new("accepted", serde_json::json!({"id": "ok"})),
        )
        .await
        .unwrap();
    drop(first_log);

    let topic_path = dir.path().join("topics").join("trigger.inbox.jsonl");
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&topic_path)
        .unwrap();
    write!(file, "{{\"id\":2,\"event\":{{\"kind\":\"partial\"").unwrap();
    drop(file);

    let reopened = FileEventLog::open(dir.path().to_path_buf(), 8).unwrap();
    let events = reopened.read_range(&topic, None, usize::MAX).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 1);
    assert_eq!(reopened.latest(&topic).await.unwrap(), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn file_backend_idempotent_append_returns_original_event() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(AnyEventLog::File(
        FileEventLog::open(dir.path().to_path_buf(), 8).unwrap(),
    ));
    exercise_idempotent_append(log).await;
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

#[test]
fn sqlite_open_skips_wal_promotion_when_existing_wal_database_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    drop(SqliteEventLog::open(path.clone(), 8).unwrap());

    let writer = Connection::open(&path).unwrap();
    writer.busy_timeout(Duration::from_millis(0)).unwrap();
    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT OR IGNORE INTO topic_heads(topic, last_id) VALUES ('held.open', 0);",
        )
        .unwrap();

    let opened = SqliteEventLog::open_with_timeout(path, 8, Duration::from_millis(25)).unwrap();
    drop(opened);
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn sqlite_read_only_open_requires_initialized_event_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    drop(Connection::open(&path).unwrap());

    let error = match SqliteEventLog::open_read_only(path, 8) {
        Ok(_) => panic!("read-only open must reject a database without the event schema"),
        Err(error) => error,
    };
    match error {
        LogError::Sqlite(message) => assert_eq!(
            message,
            "event log read-only setup error: SQLite schema event_log version 1 is not initialized"
        ),
        other => panic!("expected SQLite setup error, got {other:?}"),
    }
}

#[test]
fn sqlite_read_only_open_returns_an_initialized_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    drop(SqliteEventLog::open(path.clone(), 8).unwrap());

    let reader = SqliteEventLog::open_read_only(path, 8).expect("open initialized event log");
    assert_eq!(reader.topics().unwrap(), Vec::<Topic>::new());
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_backend_idempotent_append_returns_original_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let log = Arc::new(AnyEventLog::Sqlite(SqliteEventLog::open(path, 8).unwrap()));
    exercise_idempotent_append(log).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_backend_compacts_idempotency_keys_with_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let log = Arc::new(AnyEventLog::Sqlite(SqliteEventLog::open(path, 8).unwrap()));
    let topic = Topic::new("channel.tenant.default.compacted").unwrap();
    let mut headers = BTreeMap::new();
    headers.insert("harn.channel.id".to_string(), "event-1".to_string());
    let first = log
        .append_idempotent_by_header(
            &topic,
            "harn.channel.id",
            "event-1",
            LogEvent::new("channel.emit", serde_json::json!({"n": 1})).with_headers(headers),
        )
        .await
        .unwrap();
    log.compact(&topic, first.event_id).await.unwrap();

    let mut replacement_headers = BTreeMap::new();
    replacement_headers.insert("harn.channel.id".to_string(), "event-1".to_string());
    let replacement = log
        .append_idempotent_by_header(
            &topic,
            "harn.channel.id",
            "event-1",
            LogEvent::new("channel.emit", serde_json::json!({"n": 2}))
                .with_headers(replacement_headers),
        )
        .await
        .unwrap();
    assert!(replacement.inserted);
    assert!(replacement.event_id > first.event_id);
    assert_eq!(replacement.event.payload, serde_json::json!({"n": 2}));
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_bytes_read_preserves_payload_without_value_materialization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let topic = Topic::new("observability.action_graph").unwrap();
    let log = SqliteEventLog::open(path, 8).unwrap();
    let event_id = log
        .append(
            &topic,
            LogEvent::new(
                "snapshot",
                serde_json::json!({"nodes":[{"id":"a"}],"edges":[]}),
            ),
        )
        .await
        .unwrap();

    let events = log.read_range_bytes(&topic, None, 1).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, event_id);
    assert_eq!(
        events[0].1.payload_json().unwrap(),
        serde_json::json!({"nodes":[{"id":"a"}],"edges":[]})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_bytes_read_accepts_legacy_text_payload_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.sqlite");
    let topic = Topic::new("agent.transcript.legacy").unwrap();
    let log = SqliteEventLog::open(path, 8).unwrap();
    {
        let connection = log.connection.lock().unwrap();
        connection
            .execute(
                "INSERT INTO topic_heads(topic, last_id) VALUES (?1, 1)",
                params![topic.as_str()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events(topic, event_id, kind, payload, headers, occurred_at_ms)
                 VALUES (?1, 1, 'legacy', ?2, '{}', 1)",
                params![topic.as_str(), "{\"text\":\"old\"}"],
            )
            .unwrap();
    }

    let events = log.read_range_bytes(&topic, None, 1).await.unwrap();
    assert_eq!(
        events[0].1.payload_json().unwrap(),
        serde_json::json!({"text": "old"})
    );
    assert_eq!(
        log.read_range(&topic, None, 1).await.unwrap()[0].1.kind,
        "legacy"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn broadcast_forwarder_reports_lag_when_receiver_overflows() {
    let (sender, rx) = broadcast::channel(2);
    for i in 0..10 {
        sender
            .send((i + 1, LogEvent::new("tick", serde_json::json!({"i": i}))))
            .unwrap();
    }
    let mut stream = stream_from_broadcast(Vec::new(), None, rx, 2);

    match stream.next().await {
        Some(Err(LogError::ConsumerLagged(last_seen))) => assert_eq!(last_seen, 0),
        other => panic!("subscriber should surface lag, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn broadcast_forwarder_stops_when_consumer_drops_stream() {
    let (sender, rx) = broadcast::channel(2);
    let stream = stream_from_broadcast(Vec::new(), None, rx, 2);
    assert_eq!(sender.receiver_count(), 1);
    drop(stream);

    tokio::time::timeout(std::time::Duration::from_millis(100), async {
        while sender.receiver_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("subscription receiver should close after consumer drop");
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
