use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;

use crate::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use crate::triggers::test_util::clock;
use crate::triggers::{
    Dispatcher, ProviderPayload, SignatureStatus, TraceId, TriggerEvent, TriggerEventId,
};

use super::{DispatchError, DispatchOutcome, TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_DLQ_TOPIC};

pub const TRIGGER_STREAM_WINDOWS_TOPIC: &str = "trigger.stream.windows";
pub const TRIGGER_STREAM_STATUS_TOPIC: &str = "trigger.stream.status";
pub const TRIGGER_STREAM_GATE_TOPIC: &str = "trigger.stream.gates";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamWindowMode {
    Fixed,
    Tumbling,
    Sliding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamWindowConfig {
    pub mode: StreamWindowMode,
    pub size: usize,
    #[serde(default = "default_window_step")]
    pub step: usize,
}

impl StreamWindowConfig {
    pub fn fixed(size: usize) -> Self {
        Self {
            mode: StreamWindowMode::Fixed,
            size,
            step: size.max(1),
        }
    }

    pub fn tumbling(size: usize) -> Self {
        Self {
            mode: StreamWindowMode::Tumbling,
            size,
            step: size.max(1),
        }
    }

    pub fn sliding(size: usize, step: usize) -> Self {
        Self {
            mode: StreamWindowMode::Sliding,
            size,
            step: step.max(1),
        }
    }

    fn validate(&self) -> Result<(), DispatchError> {
        if self.size == 0 {
            return Err(DispatchError::Local(
                "stream window size must be positive".to_string(),
            ));
        }
        if self.step == 0 {
            return Err(DispatchError::Local(
                "stream window step must be positive".to_string(),
            ));
        }
        Ok(())
    }

    fn drain_after_emit(&self, pending: &mut VecDeque<TriggerEvent>) {
        match self.mode {
            StreamWindowMode::Fixed | StreamWindowMode::Tumbling => pending.clear(),
            StreamWindowMode::Sliding => {
                let remove = self.step.min(pending.len());
                pending.drain(..remove);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamOverflowPolicy {
    DropNewest,
    DropOldest,
    DeadLetterNewest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamBackpressureConfig {
    pub max_pending_events: usize,
    pub overflow: StreamOverflowPolicy,
}

impl Default for StreamBackpressureConfig {
    fn default() -> Self {
        Self {
            max_pending_events: 1024,
            overflow: StreamOverflowPolicy::DeadLetterNewest,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamThrottleConfig {
    pub max: usize,
    #[serde(with = "duration_millis")]
    pub period: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFlowConfig {
    pub debounce_by_dedupe_key: bool,
    pub throttle: Option<StreamThrottleConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamGateConfig {
    pub gate_id: String,
    pub cache_key: String,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamTriggerConfig {
    pub stream_id: String,
    pub window: StreamWindowConfig,
    #[serde(default)]
    pub backpressure: StreamBackpressureConfig,
    #[serde(default)]
    pub flow: StreamFlowConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<StreamGateConfig>,
}

impl StreamTriggerConfig {
    pub fn validate(&self) -> Result<(), DispatchError> {
        if self.stream_id.trim().is_empty() {
            return Err(DispatchError::Local(
                "stream id must be a non-empty string".to_string(),
            ));
        }
        self.window.validate()?;
        if self.backpressure.max_pending_events == 0 {
            return Err(DispatchError::Local(
                "stream max_pending_events must be positive".to_string(),
            ));
        }
        if let Some(throttle) = self.flow.throttle.as_ref() {
            if throttle.max == 0 || throttle.period.is_zero() {
                return Err(DispatchError::Local(
                    "stream throttle requires positive max and period".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamWindowEnvelope {
    pub stream_id: String,
    pub window_id: String,
    pub event_ids: Vec<String>,
    pub dedupe_keys: Vec<String>,
    pub first_occurred_at: Option<OffsetDateTime>,
    pub last_occurred_at: Option<OffsetDateTime>,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStatusSnapshot {
    pub stream_id: String,
    pub pending_events: usize,
    pub admitted_events: u64,
    pub dropped_events: u64,
    pub dead_lettered_events: u64,
    pub emitted_windows: u64,
    pub gate_passed: u64,
    pub gate_blocked: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamGateRecord {
    pub gate_id: String,
    pub cache_key: String,
    pub window_id: String,
    pub result: bool,
    pub cached: bool,
    pub reason: Option<String>,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamGateOutcome {
    Pass { reason: Option<String> },
    Block { reason: Option<String> },
}

impl StreamGateOutcome {
    fn result(&self) -> bool {
        matches!(self, Self::Pass { .. })
    }

    fn reason(&self) -> Option<String> {
        match self {
            Self::Pass { reason } | Self::Block { reason } => reason.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct StreamRuntimeState {
    pending: VecDeque<TriggerEvent>,
    throttle_hits: VecDeque<OffsetDateTime>,
    status: StreamStatusSnapshot,
}

pub struct StreamTriggerRuntime {
    config: StreamTriggerConfig,
    event_log: std::sync::Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    state: StreamRuntimeState,
}

impl StreamTriggerRuntime {
    pub fn new(
        config: StreamTriggerConfig,
        event_log: std::sync::Arc<AnyEventLog>,
        dispatcher: Dispatcher,
    ) -> Result<Self, DispatchError> {
        config.validate()?;
        let status = StreamStatusSnapshot {
            stream_id: config.stream_id.clone(),
            ..Default::default()
        };
        Ok(Self {
            config,
            event_log,
            dispatcher,
            state: StreamRuntimeState {
                status,
                ..Default::default()
            },
        })
    }

    pub fn snapshot(&self) -> StreamStatusSnapshot {
        let mut snapshot = self.state.status.clone();
        snapshot.pending_events = self.state.pending.len();
        snapshot
    }

    pub async fn push_event(
        &mut self,
        event: TriggerEvent,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        self.push_event_with_gate(event, |_| StreamGateOutcome::Pass { reason: None })
            .await
    }

    pub async fn push_event_with_gate(
        &mut self,
        event: TriggerEvent,
        gate: impl Fn(&StreamWindowEnvelope) -> StreamGateOutcome,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        self.append_status_event(
            "stream_event_received",
            json!({
                "stream_id": self.config.stream_id,
                "event_id": event.id.0,
                "dedupe_key": event.dedupe_key,
                "provider": event.provider.as_str(),
                "kind": event.kind,
            }),
        )
        .await?;

        if !self.apply_throttle(&event).await? {
            return Ok(Vec::new());
        }

        self.admit_event(event).await?;
        let windows = self.emit_ready_windows().await?;
        let mut outcomes = Vec::new();
        for (envelope, window_event) in windows {
            if !self.evaluate_gate(&envelope, &gate).await? {
                continue;
            }
            outcomes.extend(self.dispatcher.dispatch_event(window_event).await?);
        }
        Ok(outcomes)
    }

    async fn admit_event(&mut self, event: TriggerEvent) -> Result<(), DispatchError> {
        if self.config.flow.debounce_by_dedupe_key {
            let before = self.state.pending.len();
            self.state
                .pending
                .retain(|pending| pending.dedupe_key != event.dedupe_key);
            if self.state.pending.len() != before {
                self.append_status_event(
                    "stream_event_debounced",
                    json!({
                        "stream_id": self.config.stream_id,
                        "dedupe_key": event.dedupe_key,
                    }),
                )
                .await?;
            }
        }

        if self.state.pending.len() >= self.config.backpressure.max_pending_events {
            match self.config.backpressure.overflow {
                StreamOverflowPolicy::DropNewest => {
                    self.state.status.dropped_events =
                        self.state.status.dropped_events.saturating_add(1);
                    self.append_status_event(
                        "stream_event_dropped",
                        json!({
                            "stream_id": self.config.stream_id,
                            "event_id": event.id.0,
                            "reason": "backpressure_drop_newest",
                            "max_pending_events": self.config.backpressure.max_pending_events,
                        }),
                    )
                    .await?;
                    return Ok(());
                }
                StreamOverflowPolicy::DropOldest => {
                    if let Some(dropped) = self.state.pending.pop_front() {
                        self.state.status.dropped_events =
                            self.state.status.dropped_events.saturating_add(1);
                        self.append_status_event(
                            "stream_event_dropped",
                            json!({
                                "stream_id": self.config.stream_id,
                                "event_id": dropped.id.0,
                                "reason": "backpressure_drop_oldest",
                                "max_pending_events": self.config.backpressure.max_pending_events,
                            }),
                        )
                        .await?;
                    }
                }
                StreamOverflowPolicy::DeadLetterNewest => {
                    self.dead_letter_event(&event, "backpressure_queue_full")
                        .await?;
                    return Ok(());
                }
            }
        }

        self.state.status.admitted_events = self.state.status.admitted_events.saturating_add(1);
        self.state.pending.push_back(event);
        Ok(())
    }

    async fn emit_ready_windows(
        &mut self,
    ) -> Result<Vec<(StreamWindowEnvelope, TriggerEvent)>, DispatchError> {
        let mut windows = Vec::new();
        while self.state.pending.len() >= self.config.window.size {
            let members = self
                .state
                .pending
                .iter()
                .take(self.config.window.size)
                .cloned()
                .collect::<Vec<_>>();
            let envelope = self.window_envelope(&members);
            let event = self.window_event(&members, &envelope)?;
            self.append_window_event(&envelope).await?;
            self.state.status.emitted_windows = self.state.status.emitted_windows.saturating_add(1);
            windows.push((envelope, event));
            self.config.window.drain_after_emit(&mut self.state.pending);
        }
        Ok(windows)
    }

    fn window_envelope(&self, members: &[TriggerEvent]) -> StreamWindowEnvelope {
        let event_ids = members
            .iter()
            .map(|event| event.id.0.clone())
            .collect::<Vec<_>>();
        let dedupe_keys = members
            .iter()
            .map(|event| event.dedupe_key.clone())
            .collect::<Vec<_>>();
        let first_occurred_at = members
            .iter()
            .filter_map(|event| event.occurred_at)
            .min()
            .or_else(|| members.first().map(|event| event.received_at));
        let last_occurred_at = members
            .iter()
            .filter_map(|event| event.occurred_at)
            .max()
            .or_else(|| members.last().map(|event| event.received_at));
        let source = dedupe_keys.join("+");
        StreamWindowEnvelope {
            stream_id: self.config.stream_id.clone(),
            window_id: format!("stream_window:{}:{source}", self.config.stream_id),
            event_ids,
            dedupe_keys,
            first_occurred_at,
            last_occurred_at,
            replay_of_event_id: self
                .config
                .gate
                .as_ref()
                .and_then(|gate| gate.replay_of_event_id.clone()),
        }
    }

    fn window_event(
        &self,
        members: &[TriggerEvent],
        envelope: &StreamWindowEnvelope,
    ) -> Result<TriggerEvent, DispatchError> {
        let first = members
            .first()
            .ok_or_else(|| DispatchError::Local("stream window cannot be empty".to_string()))?;
        let mut headers = first.headers.clone();
        headers.insert("harn_stream_id".to_string(), envelope.stream_id.clone());
        headers.insert(
            "harn_stream_window_id".to_string(),
            envelope.window_id.clone(),
        );
        headers.insert(
            "harn_stream_source_event_ids".to_string(),
            envelope.event_ids.join(","),
        );
        let batch = members
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(DispatchError::Serde)?;
        let mut window_event = first.clone();
        window_event.id = TriggerEventId::new();
        window_event.kind = format!("{}.window", first.kind);
        window_event.dedupe_key = envelope.window_id.clone();
        window_event.trace_id = TraceId::new();
        window_event.headers = headers;
        window_event.batch = Some(batch);
        Ok(window_event)
    }

    async fn evaluate_gate(
        &mut self,
        envelope: &StreamWindowEnvelope,
        gate: &impl Fn(&StreamWindowEnvelope) -> StreamGateOutcome,
    ) -> Result<bool, DispatchError> {
        let Some(config) = self.config.gate.clone() else {
            return Ok(true);
        };
        let cache_key = format!("{}:{}", config.cache_key, envelope.dedupe_keys.join("+"));
        if let Some(record) = self.read_gate_record(&config.gate_id, &cache_key).await? {
            self.append_gate_record(StreamGateRecord {
                cached: true,
                ..record.clone()
            })
            .await?;
            if record.result {
                self.state.status.gate_passed = self.state.status.gate_passed.saturating_add(1);
            } else {
                self.state.status.gate_blocked = self.state.status.gate_blocked.saturating_add(1);
            }
            return Ok(record.result);
        }

        let outcome = gate(envelope);
        let record = StreamGateRecord {
            gate_id: config.gate_id,
            cache_key,
            window_id: envelope.window_id.clone(),
            result: outcome.result(),
            cached: false,
            reason: outcome.reason(),
            replay_of_event_id: config.replay_of_event_id,
        };
        self.append_gate_record(record.clone()).await?;
        if record.result {
            self.state.status.gate_passed = self.state.status.gate_passed.saturating_add(1);
        } else {
            self.state.status.gate_blocked = self.state.status.gate_blocked.saturating_add(1);
            self.append_status_event(
                "stream_window_gate_blocked",
                json!({
                    "stream_id": self.config.stream_id,
                    "window_id": envelope.window_id,
                    "gate_id": record.gate_id,
                    "reason": record.reason,
                }),
            )
            .await?;
        }
        Ok(record.result)
    }

    async fn apply_throttle(&mut self, event: &TriggerEvent) -> Result<bool, DispatchError> {
        let Some(throttle) = self.config.flow.throttle.clone() else {
            return Ok(true);
        };
        let now = event.occurred_at.unwrap_or(event.received_at);
        trim_hits(&mut self.state.throttle_hits, now, throttle.period);
        if self.state.throttle_hits.len() >= throttle.max {
            self.state.status.dropped_events = self.state.status.dropped_events.saturating_add(1);
            self.append_status_event(
                "stream_event_throttled",
                json!({
                    "stream_id": self.config.stream_id,
                    "event_id": event.id.0,
                    "max": throttle.max,
                    "period_ms": throttle.period.as_millis(),
                }),
            )
            .await?;
            return Ok(false);
        }
        self.state.throttle_hits.push_back(now);
        Ok(true)
    }

    async fn dead_letter_event(
        &mut self,
        event: &TriggerEvent,
        reason: &str,
    ) -> Result<(), DispatchError> {
        self.state.status.dead_lettered_events =
            self.state.status.dead_lettered_events.saturating_add(1);
        self.append_topic_event(
            TRIGGER_DLQ_TOPIC,
            "stream_dead_lettered",
            json!({
                "stream_id": self.config.stream_id,
                "event": event,
                "reason": reason,
                "pending_events": self.state.pending.len(),
                "max_pending_events": self.config.backpressure.max_pending_events,
            }),
        )
        .await?;
        self.append_status_event(
            "stream_event_dead_lettered",
            json!({
                "stream_id": self.config.stream_id,
                "event_id": event.id.0,
                "reason": reason,
            }),
        )
        .await
    }

    async fn append_window_event(
        &self,
        envelope: &StreamWindowEnvelope,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_STREAM_WINDOWS_TOPIC,
            "stream_window_emitted",
            serde_json::to_value(envelope)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        )
        .await?;
        self.append_topic_event(
            TRIGGERS_LIFECYCLE_TOPIC,
            "StreamWindowEmitted",
            json!({
                "stream_id": envelope.stream_id,
                "window_id": envelope.window_id,
                "event_ids": envelope.event_ids,
                "replay_of_event_id": envelope.replay_of_event_id,
            }),
        )
        .await
    }

    async fn append_gate_record(&self, record: StreamGateRecord) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_STREAM_GATE_TOPIC,
            "stream_gate_decision",
            serde_json::to_value(record)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        )
        .await
    }

    async fn read_gate_record(
        &self,
        gate_id: &str,
        cache_key: &str,
    ) -> Result<Option<StreamGateRecord>, DispatchError> {
        let topic =
            Topic::new(TRIGGER_STREAM_GATE_TOPIC).expect("static stream gate topic is valid");
        let records = self
            .event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .map_err(DispatchError::from)?;
        Ok(records
            .into_iter()
            .rev()
            .filter(|(_, event)| event.kind == "stream_gate_decision")
            .filter_map(|(_, event)| serde_json::from_value::<StreamGateRecord>(event.payload).ok())
            .find(|record| record.gate_id == gate_id && record.cache_key == cache_key))
    }

    async fn append_status_event(
        &self,
        kind: &str,
        mut payload: JsonValue,
    ) -> Result<(), DispatchError> {
        if let JsonValue::Object(ref mut object) = payload {
            object.insert("status".to_string(), json!(self.snapshot()));
        }
        self.append_topic_event(TRIGGER_STREAM_STATUS_TOPIC, kind, payload)
            .await
    }

    async fn append_topic_event(
        &self,
        topic: &str,
        kind: &str,
        payload: JsonValue,
    ) -> Result<(), DispatchError> {
        self.event_log
            .append(
                &Topic::new(topic).expect("static stream trigger topic is valid"),
                LogEvent::new(kind, payload),
            )
            .await
            .map(|_| ())
            .map_err(DispatchError::from)
    }
}

pub fn stream_window_summary(event: &TriggerEvent) -> Option<StreamWindowEnvelope> {
    let batch = event.batch.as_ref()?;
    let event_ids = batch
        .iter()
        .filter_map(|member| member.get("id"))
        .filter_map(JsonValue::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Some(StreamWindowEnvelope {
        stream_id: event
            .headers
            .get("harn_stream_id")
            .cloned()
            .unwrap_or_default(),
        window_id: event
            .headers
            .get("harn_stream_window_id")
            .cloned()
            .unwrap_or_else(|| event.dedupe_key.clone()),
        event_ids,
        dedupe_keys: batch
            .iter()
            .filter_map(|member| member.get("dedupe_key"))
            .filter_map(JsonValue::as_str)
            .map(ToString::to_string)
            .collect(),
        first_occurred_at: None,
        last_occurred_at: None,
        replay_of_event_id: None,
    })
}

pub fn stream_fixture_event(
    provider: impl Into<String>,
    kind: impl Into<String>,
    stream: impl Into<String>,
    offset: u64,
    payload: JsonValue,
) -> TriggerEvent {
    let provider = provider.into();
    let stream = stream.into();
    let kind = kind.into();
    let provider_id = crate::triggers::ProviderId::from(provider.clone());
    let raw = json!({
        "event": kind,
        "stream": stream,
        "offset": offset,
        "value": payload,
    });
    TriggerEvent::new(
        provider_id.clone(),
        kind.clone(),
        Some(clock::now_utc()),
        format!("{stream}:{offset}"),
        None,
        BTreeMap::new(),
        ProviderPayload::normalize(&provider_id, &kind, &BTreeMap::new(), raw).unwrap_or_else(
            |_| {
                ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Webhook(
                    crate::triggers::GenericWebhookPayload {
                        source: Some("stream_fixture".to_string()),
                        content_type: Some("application/json".to_string()),
                        raw: json!({}),
                    },
                ))
            },
        ),
        SignatureStatus::Unsigned,
    )
}

fn trim_hits(hits: &mut VecDeque<OffsetDateTime>, now: OffsetDateTime, period: Duration) {
    let period = time::Duration::try_from(period).unwrap_or_default();
    while hits.front().is_some_and(|hit| now - *hit >= period) {
        hits.pop_front();
    }
}

fn default_window_step() -> usize {
    1
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    use serde_json::json;

    use crate::event_log::{install_default_for_base_dir, EventLog};
    use crate::stdlib::register_vm_stdlib;
    use crate::triggers::dispatcher::RetryPolicy;
    use crate::triggers::TriggerRetryConfig;
    use crate::triggers::{
        clear_trigger_registry, install_manifest_triggers, ProviderId, TriggerBindingSource,
        TriggerBindingSpec, TriggerHandlerSpec,
    };
    use crate::trust_graph::AutonomyTier;
    use crate::vm::Vm;

    use super::*;

    async fn read_topic(
        log: Arc<AnyEventLog>,
        topic: &str,
    ) -> Vec<(u64, crate::event_log::LogEvent)> {
        log.read_range(&Topic::new(topic).unwrap(), None, usize::MAX)
            .await
            .unwrap()
    }

    async fn stream_dispatcher_fixture(
        source: &str,
    ) -> (tempfile::TempDir, Arc<AnyEventLog>, Dispatcher) {
        crate::reset_thread_local_state();
        clear_trigger_registry();
        let dir = tempfile::tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("event log");
        let lib_path = dir.path().join("lib.harn");
        std::fs::write(&lib_path, source).expect("write harn source");

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_source_dir(dir.path());
        let exports = vm
            .load_module_exports(&lib_path)
            .await
            .expect("load stream handler");
        let handler = exports.get("local_fn").expect("local_fn export").clone();
        install_manifest_triggers(vec![TriggerBindingSpec {
            id: "stream-window-handler".to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "stream".to_string(),
            provider: ProviderId::from("kafka"),
            autonomy_tier: AutonomyTier::ActAuto,
            handler: TriggerHandlerSpec::Local {
                raw: "local_fn".to_string(),
                closure: handler,
            },
            dispatch_priority: crate::triggers::WorkerQueuePriority::Normal,
            when: None,
            when_budget: None,
            retry: TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 1 }),
            match_events: vec!["issues.opened.window".to_string()],
            dedupe_key: Some("event.dedupe_key".to_string()),
            dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
            filter: None,
            daily_cost_usd: None,
            hourly_cost_usd: None,
            max_autonomous_decisions_per_hour: None,
            max_autonomous_decisions_per_day: None,
            on_budget_exhausted: crate::triggers::TriggerBudgetExhaustionStrategy::False,
            max_concurrent: None,
            flow_control: crate::triggers::TriggerFlowControlConfig::default(),
            aggregation: None,
            manifest_path: None,
            package_name: Some("workspace".to_string()),
            definition_fingerprint: "stream-window-handler:v1".to_string(),
        }])
        .await
        .expect("install stream trigger");

        (dir, log.clone(), Dispatcher::with_event_log(vm, log))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn chat_stream_windows_gate_and_dispatch_deterministically() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_dir, log, dispatcher) = stream_dispatcher_fixture(
                    r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> int {
  return len(event.batch ?? [])
}
"#,
                )
                .await;

                let mut runtime = StreamTriggerRuntime::new(
                    StreamTriggerConfig {
                        stream_id: "chat:triage".to_string(),
                        window: StreamWindowConfig::tumbling(2),
                        backpressure: StreamBackpressureConfig::default(),
                        flow: StreamFlowConfig::default(),
                        gate: Some(StreamGateConfig {
                            gate_id: "chat-llm-gate".to_string(),
                            cache_key: "chat:v1".to_string(),
                            replay_of_event_id: None,
                        }),
                    },
                    log.clone(),
                    dispatcher,
                )
                .unwrap();

                let calls = Rc::new(Cell::new(0));
                for offset in 0..2 {
                    let calls = calls.clone();
                    let outcomes = runtime
                        .push_event_with_gate(
                            stream_fixture_event(
                                "kafka",
                                "issues.opened",
                                "chat",
                                offset,
                                json!({"text": format!("message {offset}")}),
                            ),
                            move |_| {
                                calls.set(calls.get() + 1);
                                StreamGateOutcome::Pass {
                                    reason: Some("fixture-positive".to_string()),
                                }
                            },
                        )
                        .await
                        .unwrap();
                    if offset == 1 {
                        assert_eq!(outcomes.len(), 1);
                        assert_eq!(outcomes[0].result, Some(json!(2)));
                    } else {
                        assert!(outcomes.is_empty());
                    }
                }
                assert_eq!(calls.get(), 1);

                let gate_events = read_topic(log.clone(), TRIGGER_STREAM_GATE_TOPIC).await;
                assert_eq!(
                    gate_events
                        .iter()
                        .filter(|(_, event)| event.kind == "stream_gate_decision")
                        .count(),
                    1
                );

                let first_window_id = gate_events[0].1.payload["window_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                runtime.config.gate.as_mut().unwrap().replay_of_event_id =
                    Some(first_window_id.clone());

                for offset in 0..2 {
                    let calls = calls.clone();
                    runtime
                        .push_event_with_gate(
                            stream_fixture_event(
                                "kafka",
                                "issues.opened",
                                "chat",
                                offset,
                                json!({"text": format!("message {offset}")}),
                            ),
                            move |_| {
                                calls.set(calls.get() + 1);
                                StreamGateOutcome::Block {
                                    reason: Some("should-not-run".to_string()),
                                }
                            },
                        )
                        .await
                        .unwrap();
                }
                assert_eq!(calls.get(), 1);
                let gate_events = read_topic(log.clone(), TRIGGER_STREAM_GATE_TOPIC).await;
                assert!(gate_events.iter().any(|(_, event)| {
                    event.kind == "stream_gate_decision"
                        && event.payload["cached"] == json!(true)
                        && event.payload["window_id"] == json!(first_window_id)
                }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn issue_feed_backpressure_dead_letters_overflow() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let log = install_default_for_base_dir(dir.path()).unwrap();
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_source_dir(dir.path());
                let dispatcher = Dispatcher::with_event_log(vm, log.clone());
                let mut runtime = StreamTriggerRuntime::new(
                    StreamTriggerConfig {
                        stream_id: "issue-feed".to_string(),
                        window: StreamWindowConfig::fixed(3),
                        backpressure: StreamBackpressureConfig {
                            max_pending_events: 1,
                            overflow: StreamOverflowPolicy::DeadLetterNewest,
                        },
                        flow: StreamFlowConfig::default(),
                        gate: None,
                    },
                    log.clone(),
                    dispatcher,
                )
                .unwrap();

                runtime
                    .push_event(stream_fixture_event(
                        "kafka",
                        "issues.opened",
                        "issues",
                        1,
                        json!({"number": 1}),
                    ))
                    .await
                    .unwrap();
                runtime
                    .push_event(stream_fixture_event(
                        "kafka",
                        "issues.opened",
                        "issues",
                        2,
                        json!({"number": 2}),
                    ))
                    .await
                    .unwrap();

                let snapshot = runtime.snapshot();
                assert_eq!(snapshot.pending_events, 1);
                assert_eq!(snapshot.dead_lettered_events, 1);

                let dlq = read_topic(log.clone(), TRIGGER_DLQ_TOPIC).await;
                assert!(dlq.iter().any(|(_, event)| {
                    event.kind == "stream_dead_lettered"
                        && event.payload["reason"] == json!("backpressure_queue_full")
                }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sliding_windows_emit_overlapping_batches() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_dir, log, dispatcher) = stream_dispatcher_fixture(
                    r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> int {
  return len(event.batch ?? [])
}
"#,
                )
                .await;
                let mut runtime = StreamTriggerRuntime::new(
                    StreamTriggerConfig {
                        stream_id: "sliding".to_string(),
                        window: StreamWindowConfig::sliding(2, 1),
                        backpressure: StreamBackpressureConfig::default(),
                        flow: StreamFlowConfig::default(),
                        gate: None,
                    },
                    log.clone(),
                    dispatcher,
                )
                .unwrap();

                let mut dispatched = 0;
                for offset in 1..=3 {
                    dispatched += runtime
                        .push_event(stream_fixture_event(
                            "kafka",
                            "issues.opened",
                            "sliding",
                            offset,
                            json!({"offset": offset}),
                        ))
                        .await
                        .unwrap()
                        .len();
                }

                assert_eq!(dispatched, 2);
                assert_eq!(runtime.snapshot().emitted_windows, 2);
                let windows = read_topic(log, TRIGGER_STREAM_WINDOWS_TOPIC).await;
                assert_eq!(windows.len(), 2);
            })
            .await;
    }
}
