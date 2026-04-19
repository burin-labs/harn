use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use futures::pin_mut;
use time::OffsetDateTime;
use tokio::sync::Notify;

use crate::connectors::ConnectorError;
use crate::triggers::test_util::clock;

#[derive(Clone, Debug)]
pub(crate) struct CronSchedule {
    timezone: Tz,
    cron: Cron,
}

impl CronSchedule {
    pub(crate) fn parse(raw: impl Into<String>, timezone: Tz) -> Result<Self, ConnectorError> {
        let raw = raw.into();
        let cron = raw.parse::<Cron>().map_err(|error| {
            ConnectorError::Activation(format!("invalid cron schedule '{raw}': {error}"))
        })?;
        Ok(Self { timezone, cron })
    }

    pub(crate) fn next_tick_after(
        &self,
        after: OffsetDateTime,
    ) -> Result<OffsetDateTime, ConnectorError> {
        let mut cursor = self.to_local(after);
        let last_local = None;
        loop {
            let candidate = self
                .cron
                .find_next_occurrence(&cursor, false)
                .map_err(schedule_error)?;
            cursor = candidate + ChronoDuration::seconds(1);
            if !self
                .cron
                .is_time_matching(&candidate)
                .map_err(schedule_error)?
            {
                continue;
            }
            let candidate_local = candidate.naive_local();
            if last_local == Some(candidate_local) {
                continue;
            }
            return chrono_to_offset(candidate).map_err(schedule_error);
        }
    }

    pub(crate) fn due_ticks_between(
        &self,
        after: Option<OffsetDateTime>,
        until: OffsetDateTime,
    ) -> Result<Vec<OffsetDateTime>, ConnectorError> {
        let mut cursor = self.to_local(after.unwrap_or(until - time::Duration::minutes(1)));
        let mut last_local = after.map(|ts| self.to_local(ts).naive_local());
        let mut ticks = Vec::new();
        loop {
            let candidate = self
                .cron
                .find_next_occurrence(&cursor, false)
                .map_err(schedule_error)?;
            let candidate_offset = chrono_to_offset(candidate).map_err(schedule_error)?;
            if candidate_offset > until {
                break;
            }
            cursor = candidate + ChronoDuration::seconds(1);
            if !self
                .cron
                .is_time_matching(&candidate)
                .map_err(schedule_error)?
            {
                continue;
            }
            let candidate_local = candidate.naive_local();
            if last_local == Some(candidate_local) {
                continue;
            }
            last_local = Some(candidate_local);
            ticks.push(candidate_offset);
        }
        Ok(ticks)
    }

    fn to_local(&self, ts: OffsetDateTime) -> DateTime<Tz> {
        offset_to_utc(ts).with_timezone(&self.timezone)
    }
}

fn schedule_error(error: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::Activation(format!("cron scheduler error: {error}"))
}

fn offset_to_utc(ts: OffsetDateTime) -> DateTime<Utc> {
    Utc.timestamp_opt(ts.unix_timestamp(), ts.nanosecond())
        .single()
        .expect("offset timestamp is representable in chrono")
}

fn chrono_to_offset<TzImpl: TimeZone>(
    value: DateTime<TzImpl>,
) -> Result<OffsetDateTime, time::error::ComponentRange> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(
        value
            .timestamp_nanos_opt()
            .expect("chrono timestamp fits in i64"),
    ))
}

#[async_trait]
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
    async fn sleep_until(&self, deadline: OffsetDateTime);
}

#[derive(Debug, Default)]
pub(crate) struct RealClock;

#[async_trait]
impl Clock for RealClock {
    fn now(&self) -> OffsetDateTime {
        clock::now_utc()
    }

    async fn sleep_until(&self, deadline: OffsetDateTime) {
        let now = self.now();
        if deadline <= now {
            return;
        }
        let wait = deadline - now;
        let Ok(wait) = wait.try_into() else {
            return;
        };
        tokio::time::sleep(wait).await;
    }
}

#[async_trait]
pub(crate) trait TickHandler: Send + Sync {
    async fn on_tick(&self, tick_at: OffsetDateTime, catchup: bool) -> Result<(), ConnectorError>;
}

#[derive(Debug, Default)]
pub(crate) struct ShutdownSignal {
    stopped: AtomicBool,
    notify: Notify,
}

impl ShutdownSignal {
    pub(crate) fn request_stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        self.notify.notified().await;
    }
}

pub(crate) async fn run_tick_loop(
    schedule: CronSchedule,
    clock: Arc<dyn Clock>,
    mut cursor: OffsetDateTime,
    catchup_ticks: Vec<OffsetDateTime>,
    handler: Arc<dyn TickHandler>,
    shutdown: Arc<ShutdownSignal>,
) -> Result<(), ConnectorError> {
    for tick_at in catchup_ticks {
        if shutdown.is_stopped() {
            return Ok(());
        }
        handler.on_tick(tick_at, true).await?;
        cursor = tick_at;
    }

    loop {
        if shutdown.is_stopped() {
            return Ok(());
        }
        let next_tick = schedule.next_tick_after(cursor)?;
        if next_tick > clock.now() {
            let sleep = clock.sleep_until(next_tick);
            pin_mut!(sleep);
            tokio::select! {
                _ = &mut sleep => {}
                _ = shutdown.cancelled() => return Ok(()),
            }
        }
        if shutdown.is_stopped() {
            return Ok(());
        }
        let now = clock.now();
        let due = schedule.due_ticks_between(Some(cursor), now)?;
        if due.is_empty() {
            cursor = now;
            continue;
        }
        for tick_at in due {
            if shutdown.is_stopped() {
                return Ok(());
            }
            handler.on_tick(tick_at, false).await?;
            cursor = tick_at;
        }
    }
}
