use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use tokio::sync::Mutex;

use crate::connectors::cron::scheduler::CronSchedule;
use crate::connectors::cron::state::{CronStateStore, PersistedCronState};
use crate::connectors::cron::{looks_like_utc_offset, CatchupMode, CronConnector, CronEventSink};
use crate::connectors::{Connector, ConnectorCtx, TriggerBinding};
use crate::event_log::{AnyEventLog, EventLog, FileEventLog, MemoryEventLog, Topic};
use crate::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
};
use crate::triggers::TriggerEvent;
use crate::{InboxIndex, MetricsRegistry, ProviderId, RateLimiterFactory, TriggerKind};

use super::CRON_TICK_TOPIC;

struct RecordingSink {
    events: Mutex<Vec<TriggerEvent>>,
}

impl RecordingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
        })
    }

    async fn take(&self) -> Vec<TriggerEvent> {
        self.events.lock().await.clone()
    }
}

#[async_trait]
impl CronEventSink for RecordingSink {
    async fn emit(&self, event: TriggerEvent) -> Result<(), crate::connectors::ConnectorError> {
        self.events.lock().await.push(event);
        Ok(())
    }
}

fn binding(id: &str, schedule: &str, timezone: &str, catchup_mode: CatchupMode) -> TriggerBinding {
    TriggerBinding {
        provider: ProviderId::from("cron"),
        kind: TriggerKind::from("cron"),
        binding_id: id.to_string(),
        config: json!({
            "schedule": schedule,
            "timezone": timezone,
            "catchup_mode": catchup_mode,
        }),
    }
}

fn ctx(event_log: Arc<AnyEventLog>) -> ConnectorCtx {
    ConnectorCtx {
        event_log,
        secrets: Arc::new(FakeSecretProvider),
        inbox: Arc::new(InboxIndex),
        metrics: Arc::new(MetricsRegistry),
        rate_limiter: Arc::new(RateLimiterFactory::default()),
    }
}

fn parse(ts: &str) -> OffsetDateTime {
    OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).unwrap()
}

struct FakeSecretProvider;

#[async_trait]
impl SecretProvider for FakeSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        Err(SecretError::NotFound {
            provider: self.namespace().to_string(),
            id: id.clone(),
        })
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Ok(())
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        Ok(RotationHandle {
            provider: self.namespace().to_string(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        "cron"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

#[test]
fn midnight_schedule_tracks_new_york_local_midnight_in_dst_and_standard_time() {
    let schedule = CronSchedule::parse("0 0 * * *", "America/New_York".parse().unwrap()).unwrap();

    let january = schedule
        .next_tick_after(parse("2026-01-15T04:59:00Z"))
        .unwrap();
    let july = schedule
        .next_tick_after(parse("2026-07-15T03:59:00Z"))
        .unwrap();

    assert_eq!(january, parse("2026-01-15T05:00:00Z"));
    assert_eq!(july, parse("2026-07-15T04:00:00Z"));
}

#[test]
fn fallback_hour_fires_only_once() {
    let schedule = CronSchedule::parse("0 1 * * *", "America/New_York".parse().unwrap()).unwrap();
    let due = schedule
        .due_ticks_between(
            Some(parse("2026-11-01T04:59:00Z")),
            parse("2026-11-01T07:01:00Z"),
        )
        .unwrap();

    assert_eq!(due, vec![parse("2026-11-01T05:00:00Z")]);
}

#[test]
fn spring_forward_gap_does_not_fire_missing_hour() {
    let schedule = CronSchedule::parse("0 2 * * *", "America/New_York".parse().unwrap()).unwrap();
    let due = schedule
        .due_ticks_between(
            Some(parse("2026-03-08T06:59:00Z")),
            parse("2026-03-08T08:01:00Z"),
        )
        .unwrap();

    assert!(due.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn durable_state_round_trips_through_event_log() {
    let tmp = tempfile::tempdir().unwrap();
    let first = Arc::new(AnyEventLog::File(
        FileEventLog::open(tmp.path().to_path_buf(), 32).unwrap(),
    ));
    let store = CronStateStore::new(first);
    store
        .persist(PersistedCronState {
            trigger_id: "nightly".to_string(),
            last_fired_at: parse("2026-04-19T00:10:00Z"),
        })
        .await
        .unwrap();

    let reopened = Arc::new(AnyEventLog::File(
        FileEventLog::open(tmp.path().to_path_buf(), 32).unwrap(),
    ));
    let restored = CronStateStore::new(reopened)
        .load("nightly")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(restored.last_fired_at, parse("2026-04-19T00:10:00Z"));
}

#[tokio::test(flavor = "current_thread")]
async fn catchup_skip_drops_missed_ticks() {
    let clock = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:10:00Z"));
    let sink = RecordingSink::new();
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let store = CronStateStore::new(log.clone());
    store
        .persist(PersistedCronState {
            trigger_id: "hourly".to_string(),
            last_fired_at: parse("2026-04-19T00:00:00Z"),
        })
        .await
        .unwrap();

    let mut connector = CronConnector::with_clock_and_sink(clock.clone(), sink.clone());
    connector.init(ctx(log)).await.unwrap();
    connector
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::Skip)])
        .await
        .unwrap();
    tokio::task::yield_now().await;

    assert!(sink.take().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn catchup_all_replays_every_missed_tick_in_order() {
    let clock = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:10:00Z"));
    let sink = RecordingSink::new();
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let store = CronStateStore::new(log.clone());
    store
        .persist(PersistedCronState {
            trigger_id: "hourly".to_string(),
            last_fired_at: parse("2026-04-19T00:00:00Z"),
        })
        .await
        .unwrap();

    let mut connector = CronConnector::with_clock_and_sink(clock.clone(), sink.clone());
    connector.init(ctx(log)).await.unwrap();
    connector
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::All)])
        .await
        .unwrap();
    tokio::task::yield_now().await;

    let events = sink.take().await;
    assert_eq!(events.len(), 10);
    assert_eq!(events[0].occurred_at, Some(parse("2026-04-19T00:01:00Z")));
    assert_eq!(events[9].occurred_at, Some(parse("2026-04-19T00:10:00Z")));
}

#[tokio::test(flavor = "current_thread")]
async fn catchup_latest_replays_only_the_most_recent_tick() {
    let clock = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:10:00Z"));
    let sink = RecordingSink::new();
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let store = CronStateStore::new(log.clone());
    store
        .persist(PersistedCronState {
            trigger_id: "hourly".to_string(),
            last_fired_at: parse("2026-04-19T00:00:00Z"),
        })
        .await
        .unwrap();

    let mut connector = CronConnector::with_clock_and_sink(clock.clone(), sink.clone());
    connector.init(ctx(log)).await.unwrap();
    connector
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::Latest)])
        .await
        .unwrap();
    tokio::task::yield_now().await;

    let events = sink.take().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].occurred_at, Some(parse("2026-04-19T00:10:00Z")));
}

#[tokio::test(flavor = "current_thread")]
async fn restart_uses_durable_state_to_avoid_duplicate_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().to_path_buf();
    let sink_one = RecordingSink::new();
    let clock_one = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:00:30Z"));
    let log_one = Arc::new(AnyEventLog::File(
        FileEventLog::open(path.clone(), 32).unwrap(),
    ));

    let mut first = CronConnector::with_clock_and_sink(clock_one.clone(), sink_one.clone());
    first.init(ctx(log_one)).await.unwrap();
    first
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::Skip)])
        .await
        .unwrap();
    clock_one.advance(Duration::seconds(30)).await;
    tokio::task::yield_now().await;
    assert_eq!(sink_one.take().await.len(), 1);
    drop(first);

    let sink_two = RecordingSink::new();
    let clock_two = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:01:30Z"));
    let log_two = Arc::new(AnyEventLog::File(FileEventLog::open(path, 32).unwrap()));
    let mut second = CronConnector::with_clock_and_sink(clock_two.clone(), sink_two.clone());
    second.init(ctx(log_two)).await.unwrap();
    second
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::Skip)])
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert!(sink_two.take().await.is_empty());
    clock_two.advance(Duration::seconds(30)).await;
    tokio::task::yield_now().await;

    let events = sink_two.take().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].occurred_at, Some(parse("2026-04-19T00:02:00Z")));
}

#[tokio::test(flavor = "current_thread")]
async fn default_sink_writes_trigger_events_to_event_log() {
    let clock = crate::connectors::test_util::MockClock::new(parse("2026-04-19T00:00:30Z"));
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut connector = CronConnector::with_clock(clock.clone());
    connector.init(ctx(log.clone())).await.unwrap();
    connector
        .activate(&[binding("hourly", "* * * * *", "UTC", CatchupMode::Skip)])
        .await
        .unwrap();
    clock.advance(Duration::seconds(30)).await;
    tokio::task::yield_now().await;

    let topic = Topic::new(CRON_TICK_TOPIC).unwrap();
    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn utc_offset_detection_rejects_offset_style_timezones() {
    assert!(looks_like_utc_offset("+02:00"));
    assert!(looks_like_utc_offset("UTC-5"));
    assert!(!looks_like_utc_offset("UTC"));
    assert!(!looks_like_utc_offset("America/New_York"));
}
