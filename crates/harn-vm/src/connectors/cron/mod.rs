use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;
use tokio::task::JoinHandle;

use crate::connectors::{
    ActivationHandle, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    ProviderPayloadSchema, RawInbound, TriggerBinding, TriggerKind,
};
use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use crate::triggers::event::KnownProviderPayload;
use crate::triggers::{
    CronEventPayload, ProviderId, ProviderPayload, SignatureStatus, TriggerEvent,
    DEFAULT_INBOX_RETENTION_DAYS,
};

use self::scheduler::{run_tick_loop, Clock, CronSchedule, RealClock, TickHandler};
use self::state::{CronStateStore, PersistedCronState};

pub(crate) mod scheduler;
pub(crate) mod state;

#[cfg(test)]
mod tests;

pub const CRON_TICK_TOPIC: &str = "connectors.cron.tick";
const TEST_FAIL_AFTER_EMIT_ENV: &str = "HARN_TEST_CRON_FAIL_AFTER_EMIT";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchupMode {
    #[default]
    Skip,
    All,
    Latest,
}

pub struct CronConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    client: Arc<CronClient>,
    ctx: Mutex<Option<ConnectorCtx>>,
    clock: Arc<dyn Clock>,
    sink_override: Option<Arc<dyn CronEventSink>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl CronConnector {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(RealClock))
    }

    pub(crate) fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            provider_id: ProviderId::from("cron"),
            kinds: vec![TriggerKind::from("cron")],
            client: Arc::new(CronClient),
            ctx: Mutex::new(None),
            clock,
            sink_override: None,
            tasks: Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_clock_and_sink(clock: Arc<dyn Clock>, sink: Arc<dyn CronEventSink>) -> Self {
        let mut connector = Self::with_clock(clock);
        connector.sink_override = Some(sink);
        connector
    }

    fn context(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.ctx
            .lock()
            .expect("cron connector context mutex poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::Activation(
                    "cron connector must be initialized before activation".to_string(),
                )
            })
    }
}

impl Default for CronConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CronConnector {
    fn drop(&mut self) {
        let mut tasks = self
            .tasks
            .lock()
            .expect("cron connector tasks mutex poisoned");
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

#[async_trait]
impl Connector for CronConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        *self
            .ctx
            .lock()
            .expect("cron connector context mutex poisoned") = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<ActivationHandle, ConnectorError> {
        let ctx = self.context()?;
        let state_store = Arc::new(CronStateStore::new(ctx.event_log.clone()));
        let sink: Arc<dyn CronEventSink> = self.sink_override.clone().unwrap_or_else(|| {
            Arc::new(EventLogCronEventSink::new(
                ctx.event_log.clone(),
                ctx.inbox.clone(),
            ))
        });
        {
            let mut tasks = self
                .tasks
                .lock()
                .expect("cron connector tasks mutex poisoned");
            for task in tasks.drain(..) {
                task.abort();
            }
        }

        for binding in bindings {
            let trigger = CronTrigger::from_binding(binding)?;
            let clock = self.clock.clone();
            let sink = sink.clone();
            let state_store = state_store.clone();
            let last_fired = state_store
                .load(&trigger.trigger_id)
                .await?
                .map(|state| state.last_fired_at);
            let now = clock.now();
            let catchup_ticks = match trigger.catchup_mode {
                CatchupMode::Skip => Vec::new(),
                CatchupMode::All => trigger.schedule.due_ticks_between(last_fired, now)?,
                CatchupMode::Latest => trigger
                    .schedule
                    .due_ticks_between(last_fired, now)?
                    .into_iter()
                    .last()
                    .into_iter()
                    .collect(),
            };
            let cursor = match trigger.catchup_mode {
                CatchupMode::Skip => now,
                _ => last_fired.unwrap_or(now),
            };
            let handler = Arc::new(CronTaskHandler {
                trigger,
                sink,
                state_store,
            });
            let task = tokio::spawn(async move {
                let _ = run_tick_loop(
                    handler.trigger.schedule.clone(),
                    clock,
                    cursor,
                    catchup_ticks,
                    handler,
                )
                .await;
            });
            self.tasks
                .lock()
                .expect("cron connector tasks mutex poisoned")
                .push(task);
        }

        Ok(ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    fn normalize_inbound(&self, _raw: RawInbound) -> Result<TriggerEvent, ConnectorError> {
        Err(ConnectorError::Unsupported(
            "cron is an in-process scheduler and does not accept inbound payloads".to_string(),
        ))
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        ProviderPayloadSchema::named("CronEventPayload")
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        self.client.clone()
    }
}

#[derive(Debug)]
struct CronClient;

#[async_trait]
impl ConnectorClient for CronClient {
    async fn call(&self, method: &str, _args: JsonValue) -> Result<JsonValue, ClientError> {
        Err(ClientError::MethodNotFound(format!(
            "cron connector does not implement outbound method '{method}'"
        )))
    }
}

#[async_trait]
pub(crate) trait CronEventSink: Send + Sync {
    async fn emit(
        &self,
        binding_id: &str,
        retention: StdDuration,
        event: TriggerEvent,
    ) -> Result<(), ConnectorError>;
}

struct EventLogCronEventSink {
    event_log: Arc<AnyEventLog>,
    inbox: Arc<crate::triggers::InboxIndex>,
    topic: Topic,
}

impl EventLogCronEventSink {
    fn new(event_log: Arc<AnyEventLog>, inbox: Arc<crate::triggers::InboxIndex>) -> Self {
        Self {
            event_log,
            inbox,
            topic: Topic::new(CRON_TICK_TOPIC).expect("cron tick topic is valid"),
        }
    }
}

#[async_trait]
impl CronEventSink for EventLogCronEventSink {
    async fn emit(
        &self,
        binding_id: &str,
        retention: StdDuration,
        event: TriggerEvent,
    ) -> Result<(), ConnectorError> {
        if !self
            .inbox
            .insert_if_new(binding_id, &event.dedupe_key, retention)
            .await?
        {
            return Ok(());
        }
        let payload = serde_json::to_value(&event).map_err(ConnectorError::from)?;
        self.event_log
            .append(&self.topic, LogEvent::new("trigger_event", payload))
            .await
            .map_err(ConnectorError::from)?;
        Ok(())
    }
}

#[derive(Clone)]
struct CronTaskHandler {
    trigger: CronTrigger,
    sink: Arc<dyn CronEventSink>,
    state_store: Arc<CronStateStore>,
}

#[async_trait]
impl TickHandler for CronTaskHandler {
    async fn on_tick(&self, tick_at: OffsetDateTime, catchup: bool) -> Result<(), ConnectorError> {
        let event = self.trigger.to_event(tick_at, catchup);
        self.sink
            .emit(
                &self.trigger.trigger_id,
                self.trigger.dedupe_retention,
                event,
            )
            .await?;
        maybe_fail_after_emit();
        self.state_store
            .persist(PersistedCronState {
                trigger_id: self.trigger.trigger_id.clone(),
                last_fired_at: tick_at,
            })
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct CronTrigger {
    trigger_id: String,
    schedule_raw: String,
    timezone_raw: String,
    schedule: CronSchedule,
    catchup_mode: CatchupMode,
    dedupe_retention: StdDuration,
}

impl CronTrigger {
    fn from_binding(binding: &TriggerBinding) -> Result<Self, ConnectorError> {
        let config: CronBindingConfig =
            serde_json::from_value(binding.config.clone()).map_err(ConnectorError::from)?;
        let timezone = parse_iana_timezone(&config.timezone)?;
        Ok(Self {
            trigger_id: binding.binding_id.clone(),
            schedule_raw: config.schedule.clone(),
            timezone_raw: config.timezone.clone(),
            schedule: CronSchedule::parse(config.schedule, timezone)?,
            catchup_mode: config.catchup_mode,
            dedupe_retention: StdDuration::from_secs(
                u64::from(config.retention_days.max(1)) * 24 * 60 * 60,
            ),
        })
    }

    fn to_event(&self, tick_at: OffsetDateTime, catchup: bool) -> TriggerEvent {
        let payload = ProviderPayload::Known(KnownProviderPayload::Cron(CronEventPayload {
            cron_id: Some(self.trigger_id.clone()),
            schedule: Some(self.schedule_raw.clone()),
            tick_at,
            raw: json!({
                "catchup": catchup,
                "timezone": self.timezone_raw,
            }),
        }));
        TriggerEvent::new(
            ProviderId::from("cron"),
            "tick",
            Some(tick_at),
            format!("cron:{}:{}", self.trigger_id, tick_at.unix_timestamp()),
            None,
            BTreeMap::new(),
            payload,
            SignatureStatus::Unsigned,
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
struct CronBindingConfig {
    schedule: String,
    timezone: String,
    #[serde(default, alias = "catch_up", alias = "catchup")]
    catchup_mode: CatchupMode,
    #[serde(default = "default_retention_days")]
    retention_days: u32,
}

fn default_retention_days() -> u32 {
    DEFAULT_INBOX_RETENTION_DAYS
}

fn parse_iana_timezone(raw: &str) -> Result<chrono_tz::Tz, ConnectorError> {
    if looks_like_utc_offset(raw) {
        return Err(ConnectorError::Activation(format!(
            "invalid cron timezone '{raw}': use an IANA timezone name like 'America/New_York', not a UTC offset"
        )));
    }
    raw.parse::<chrono_tz::Tz>().map_err(|error| {
        ConnectorError::Activation(format!("invalid cron timezone '{raw}': {error}"))
    })
}

pub(crate) fn looks_like_utc_offset(raw: &str) -> bool {
    let value = raw.trim();
    if let Some(rest) = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("utc"))
        .or_else(|| value.strip_prefix("GMT"))
        .or_else(|| value.strip_prefix("gmt"))
    {
        return rest.starts_with('+') || rest.starts_with('-');
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 3 || !matches!(chars[0], '+' | '-') {
        return false;
    }
    chars[1..]
        .iter()
        .all(|ch| ch.is_ascii_digit() || *ch == ':')
}

fn maybe_fail_after_emit() {
    if std::env::var_os(TEST_FAIL_AFTER_EMIT_ENV).is_some() {
        std::process::exit(86);
    }
}
