use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "otel")]
use std::time::Instant;

use futures::{pin_mut, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tracing::Instrument as _;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogError, LogEvent, Topic};
use crate::llm::trigger_predicate::{start_predicate_evaluation, PredicateCacheEntry};
use crate::llm::vm_value_to_json;
use crate::orchestration::{
    append_action_graph_update, RunActionGraphEdgeRecord, RunActionGraphNodeRecord,
    RunObservabilityRecord, ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH, ACTION_GRAPH_EDGE_KIND_DLQ_MOVE,
    ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE, ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN,
    ACTION_GRAPH_EDGE_KIND_RETRY, ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    ACTION_GRAPH_NODE_KIND_A2A_HOP, ACTION_GRAPH_NODE_KIND_DISPATCH, ACTION_GRAPH_NODE_KIND_DLQ,
    ACTION_GRAPH_NODE_KIND_RETRY, ACTION_GRAPH_NODE_KIND_TRIGGER,
    ACTION_GRAPH_NODE_KIND_TRIGGER_PREDICATE, ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE,
};
use crate::stdlib::json_to_vm_value;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{error_to_category, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use self::uri::DispatchUri;
use super::registry::matching_bindings;
use super::registry::{TriggerBinding, TriggerHandlerSpec};
use super::{
    begin_in_flight, finish_in_flight, TriggerDispatchOutcome, TriggerEvent,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC,
    TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};
use flow_control::{BatchDecision, ConcurrencyPermit, FlowControlManager};

mod flow_control;
pub mod retry;
pub mod uri;

pub use retry::{RetryPolicy, TriggerRetryConfig, DEFAULT_MAX_ATTEMPTS};

thread_local! {
    static ACTIVE_DISPATCHER_STATE: RefCell<Option<Arc<DispatcherRuntimeState>>> = const { RefCell::new(None) };
    static ACTIVE_DISPATCH_CONTEXT: RefCell<Option<DispatchContext>> = const { RefCell::new(None) };
    static ACTIVE_DISPATCH_WAIT_LEASE: RefCell<Option<DispatchWaitLease>> = const { RefCell::new(None) };
    #[cfg(test)]
    static TEST_INBOX_DEQUEUED_SIGNAL: RefCell<Option<tokio::sync::oneshot::Sender<()>>> = const { RefCell::new(None) };
}

tokio::task_local! {
    static ACTIVE_DISPATCH_IS_REPLAY: bool;
}

#[derive(Clone, Debug)]
pub(crate) struct DispatchContext {
    pub trigger_event: TriggerEvent,
    pub replay_of_event_id: Option<String>,
    pub binding_id: String,
    pub binding_version: u32,
    pub agent_id: String,
    pub action: String,
    pub autonomy_tier: AutonomyTier,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PredicateCacheRecord {
    trigger_id: String,
    event_id: String,
    entries: Vec<PredicateCacheEntry>,
}

#[derive(Clone, Debug, Default)]
struct PredicateEvaluationRecord {
    result: bool,
    cost_usd: f64,
    tokens: u64,
    latency_ms: u64,
    cached: bool,
    reason: Option<String>,
}

pub(crate) fn current_dispatch_context() -> Option<DispatchContext> {
    ACTIVE_DISPATCH_CONTEXT.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_dispatch_is_replay() -> bool {
    ACTIVE_DISPATCH_IS_REPLAY
        .try_with(|is_replay| *is_replay)
        .unwrap_or(false)
}

pub(crate) fn current_dispatch_wait_lease() -> Option<DispatchWaitLease> {
    ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| slot.borrow().clone())
}

#[derive(Clone)]
pub struct Dispatcher {
    base_vm: Rc<Vm>,
    event_log: Arc<AnyEventLog>,
    cancel_tx: broadcast::Sender<()>,
    state: Arc<DispatcherRuntimeState>,
    metrics: Option<Arc<crate::MetricsRegistry>>,
}

#[derive(Debug)]
struct DispatcherRuntimeState {
    in_flight: AtomicU64,
    retry_queue_depth: AtomicU64,
    dlq: Mutex<Vec<DlqEntry>>,
    cancel_tokens: Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>,
    shutting_down: std::sync::atomic::AtomicBool,
    idle_notify: Notify,
    flow_control: FlowControlManager,
}

impl DispatcherRuntimeState {
    fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self {
            in_flight: AtomicU64::new(0),
            retry_queue_depth: AtomicU64::new(0),
            dlq: Mutex::new(Vec::new()),
            cancel_tokens: Mutex::new(Vec::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            idle_notify: Notify::new(),
            flow_control: FlowControlManager::new(event_log),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatcherStatsSnapshot {
    pub in_flight: u64,
    pub retry_queue_depth: u64,
    pub dlq_depth: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    Succeeded,
    Failed,
    Dlq,
    Skipped,
    Waiting,
    Cancelled,
}

impl DispatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Dlq => "dlq",
            Self::Skipped => "skipped",
            Self::Waiting => "waiting",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchOutcome {
    pub trigger_id: String,
    pub binding_key: String,
    pub event_id: String,
    pub attempt_count: u32,
    pub status: DispatchStatus,
    pub handler_kind: String,
    pub target_uri: String,
    pub replay_of_event_id: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboxEnvelope {
    pub trigger_id: Option<String>,
    pub binding_version: Option<u32>,
    pub event: TriggerEvent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatcherDrainReport {
    pub drained: bool,
    pub in_flight: u64,
    pub retry_queue_depth: u64,
    pub dlq_depth: u64,
}

impl Default for DispatchOutcome {
    fn default() -> Self {
        Self {
            trigger_id: String::new(),
            binding_key: String::new(),
            event_id: String::new(),
            attempt_count: 0,
            status: DispatchStatus::Failed,
            handler_kind: String::new(),
            target_uri: String::new(),
            replay_of_event_id: None,
            result: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchAttemptRecord {
    pub trigger_id: String,
    pub binding_key: String,
    pub event_id: String,
    pub attempt: u32,
    pub handler_kind: String,
    pub started_at: String,
    pub completed_at: String,
    pub outcome: String,
    pub error_msg: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DispatchCancelRequest {
    pub binding_key: String,
    pub event_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: time::OffsetDateTime,
    #[serde(default)]
    pub requested_by: Option<String>,
    #[serde(default)]
    pub audit_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DlqEntry {
    pub trigger_id: String,
    pub binding_key: String,
    pub event: TriggerEvent,
    pub attempt_count: u32,
    pub final_error: String,
    pub attempts: Vec<DispatchAttemptRecord>,
}

#[derive(Clone, Debug)]
struct SingletonLease {
    gate: String,
    held: bool,
}

#[derive(Clone, Debug)]
struct ConcurrencyLease {
    gate: String,
    max: u32,
    priority_rank: usize,
    permit: Option<ConcurrencyPermit>,
}

#[derive(Default, Debug)]
struct AcquiredFlowControl {
    singleton: Option<SingletonLease>,
    concurrency: Option<ConcurrencyLease>,
}

#[derive(Clone)]
pub(crate) struct DispatchWaitLease {
    state: Arc<DispatcherRuntimeState>,
    acquired: Arc<AsyncMutex<AcquiredFlowControl>>,
    suspended: Arc<AtomicBool>,
}

impl DispatchWaitLease {
    fn new(
        state: Arc<DispatcherRuntimeState>,
        acquired: Arc<AsyncMutex<AcquiredFlowControl>>,
    ) -> Self {
        Self {
            state,
            acquired,
            suspended: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) async fn suspend(&self) -> Result<(), DispatchError> {
        if self.suspended.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (singleton_gate, concurrency_permit) = {
            let mut acquired = self.acquired.lock().await;
            let singleton_gate = acquired.singleton.as_mut().and_then(|lease| {
                if lease.held {
                    lease.held = false;
                    Some(lease.gate.clone())
                } else {
                    None
                }
            });
            let concurrency_permit = acquired
                .concurrency
                .as_mut()
                .and_then(|lease| lease.permit.take());
            (singleton_gate, concurrency_permit)
        };

        if let Some(gate) = singleton_gate {
            self.state
                .flow_control
                .release_singleton(&gate)
                .await
                .map_err(DispatchError::from)?;
        }
        if let Some(permit) = concurrency_permit {
            self.state
                .flow_control
                .release_concurrency(permit)
                .await
                .map_err(DispatchError::from)?;
        }
        Ok(())
    }

    pub(crate) async fn resume(&self) -> Result<(), DispatchError> {
        if !self.suspended.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        let singleton_gate = {
            let acquired = self.acquired.lock().await;
            acquired.singleton.as_ref().and_then(|lease| {
                if lease.held {
                    None
                } else {
                    Some(lease.gate.clone())
                }
            })
        };
        if let Some(gate) = singleton_gate {
            self.state
                .flow_control
                .acquire_singleton(&gate)
                .await
                .map_err(DispatchError::from)?;
            let mut acquired = self.acquired.lock().await;
            if let Some(lease) = acquired.singleton.as_mut() {
                lease.held = true;
            }
        }

        let concurrency_spec = {
            let acquired = self.acquired.lock().await;
            acquired.concurrency.as_ref().and_then(|lease| {
                if lease.permit.is_some() {
                    None
                } else {
                    Some((lease.gate.clone(), lease.max, lease.priority_rank))
                }
            })
        };
        if let Some((gate, max, priority_rank)) = concurrency_spec {
            let permit = self
                .state
                .flow_control
                .acquire_concurrency(&gate, max, priority_rank)
                .await
                .map_err(DispatchError::from)?;
            let mut acquired = self.acquired.lock().await;
            if let Some(lease) = acquired.concurrency.as_mut() {
                lease.permit = Some(permit);
            }
        }
        Ok(())
    }
}

enum FlowControlOutcome {
    Dispatch {
        event: Box<TriggerEvent>,
        acquired: AcquiredFlowControl,
    },
    Skip {
        reason: String,
    },
}

#[derive(Clone, Debug)]
enum DispatchSkipStage {
    Predicate,
    FlowControl,
}

#[derive(Debug)]
pub enum DispatchError {
    EventLog(String),
    Registry(String),
    Serde(String),
    Local(String),
    A2a(String),
    Denied(String),
    Timeout(String),
    Waiting(String),
    Cancelled(String),
    NotImplemented(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLog(message)
            | Self::Registry(message)
            | Self::Serde(message)
            | Self::Local(message)
            | Self::A2a(message)
            | Self::Denied(message)
            | Self::Timeout(message)
            | Self::Waiting(message)
            | Self::Cancelled(message)
            | Self::NotImplemented(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DispatchError {}

impl DispatchError {
    fn retryable(&self) -> bool {
        !matches!(
            self,
            Self::Cancelled(_) | Self::Denied(_) | Self::NotImplemented(_) | Self::Waiting(_)
        )
    }
}

impl DispatchSkipStage {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Predicate => "predicate",
            Self::FlowControl => "flow_control",
        }
    }
}

impl From<LogError> for DispatchError {
    fn from(value: LogError) -> Self {
        Self::EventLog(value.to_string())
    }
}

pub async fn append_dispatch_cancel_request(
    event_log: &Arc<AnyEventLog>,
    request: &DispatchCancelRequest,
) -> Result<u64, DispatchError> {
    let topic = Topic::new(TRIGGER_CANCEL_REQUESTS_TOPIC)
        .expect("static trigger cancel topic should always be valid");
    event_log
        .append(
            &topic,
            LogEvent::new(
                "dispatch_cancel_requested",
                serde_json::to_value(request)
                    .map_err(|error| DispatchError::Serde(error.to_string()))?,
            ),
        )
        .await
        .map_err(DispatchError::from)
}

impl Dispatcher {
    pub fn event_log_handle(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub fn new(base_vm: Vm) -> Result<Self, DispatchError> {
        let event_log = active_event_log().ok_or_else(|| {
            DispatchError::EventLog("dispatcher requires an active event log".to_string())
        })?;
        Ok(Self::with_event_log(base_vm, event_log))
    }

    pub fn with_event_log(base_vm: Vm, event_log: Arc<AnyEventLog>) -> Self {
        Self::with_event_log_and_metrics(base_vm, event_log, None)
    }

    pub fn with_event_log_and_metrics(
        base_vm: Vm,
        event_log: Arc<AnyEventLog>,
        metrics: Option<Arc<crate::MetricsRegistry>>,
    ) -> Self {
        let state = Arc::new(DispatcherRuntimeState::new(event_log.clone()));
        ACTIVE_DISPATCHER_STATE.with(|slot| {
            *slot.borrow_mut() = Some(state.clone());
        });
        let (cancel_tx, _) = broadcast::channel(32);
        Self {
            base_vm: Rc::new(base_vm),
            event_log,
            cancel_tx,
            state,
            metrics,
        }
    }

    pub fn snapshot(&self) -> DispatcherStatsSnapshot {
        DispatcherStatsSnapshot {
            in_flight: self.state.in_flight.load(Ordering::Relaxed),
            retry_queue_depth: self.state.retry_queue_depth.load(Ordering::Relaxed),
            dlq_depth: self
                .state
                .dlq
                .lock()
                .expect("dispatcher dlq poisoned")
                .len() as u64,
        }
    }

    pub fn dlq_entries(&self) -> Vec<DlqEntry> {
        self.state
            .dlq
            .lock()
            .expect("dispatcher dlq poisoned")
            .clone()
    }

    pub fn shutdown(&self) {
        self.state.shutting_down.store(true, Ordering::SeqCst);
        for token in self
            .state
            .cancel_tokens
            .lock()
            .expect("dispatcher cancel tokens poisoned")
            .iter()
        {
            token.store(true, Ordering::SeqCst);
        }
        let _ = self.cancel_tx.send(());
    }

    pub async fn enqueue(&self, event: TriggerEvent) -> Result<u64, DispatchError> {
        self.enqueue_targeted(None, None, event).await
    }

    pub async fn enqueue_targeted(
        &self,
        trigger_id: Option<String>,
        binding_version: Option<u32>,
        event: TriggerEvent,
    ) -> Result<u64, DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC)
            .expect("static trigger inbox envelopes topic is valid");
        let headers = event_headers(&event, None, None, None);
        let payload = serde_json::to_value(InboxEnvelope {
            trigger_id,
            binding_version,
            event,
        })
        .map_err(|error| DispatchError::Serde(error.to_string()))?;
        self.event_log
            .append(
                &topic,
                LogEvent::new("event_ingested", payload).with_headers(headers),
            )
            .await
            .map_err(DispatchError::from)
    }

    pub async fn run(&self) -> Result<(), DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC)
            .expect("static trigger inbox envelopes topic is valid");
        let start_from = self.event_log.latest(&topic).await?;
        let stream = self.event_log.clone().subscribe(&topic, start_from).await?;
        pin_mut!(stream);
        let mut cancel_rx = self.cancel_tx.subscribe();

        loop {
            tokio::select! {
                received = stream.next() => {
                    let Some(received) = received else {
                        break;
                    };
                    let (_, event) = received.map_err(DispatchError::from)?;
                    if event.kind != "event_ingested" {
                        continue;
                    }
                    let parent_headers = event.headers.clone();
                    let envelope: InboxEnvelope = serde_json::from_value(event.payload)
                        .map_err(|error| DispatchError::Serde(error.to_string()))?;
                    notify_test_inbox_dequeued();
                    let _ = self
                        .dispatch_inbox_envelope_with_headers(envelope, Some(&parent_headers))
                        .await;
                }
                _ = recv_cancel(&mut cancel_rx) => break,
            }
        }

        Ok(())
    }

    pub async fn drain(&self, timeout: Duration) -> Result<DispatcherDrainReport, DispatchError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let snapshot = self.snapshot();
            if snapshot.in_flight == 0 && snapshot.retry_queue_depth == 0 {
                return Ok(DispatcherDrainReport {
                    drained: true,
                    in_flight: snapshot.in_flight,
                    retry_queue_depth: snapshot.retry_queue_depth,
                    dlq_depth: snapshot.dlq_depth,
                });
            }

            let notified = self.state.idle_notify.notified();
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(DispatcherDrainReport {
                    drained: false,
                    in_flight: snapshot.in_flight,
                    retry_queue_depth: snapshot.retry_queue_depth,
                    dlq_depth: snapshot.dlq_depth,
                });
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                let snapshot = self.snapshot();
                return Ok(DispatcherDrainReport {
                    drained: false,
                    in_flight: snapshot.in_flight,
                    retry_queue_depth: snapshot.retry_queue_depth,
                    dlq_depth: snapshot.dlq_depth,
                });
            }
        }
    }

    pub async fn dispatch_inbox_envelope(
        &self,
        envelope: InboxEnvelope,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        self.dispatch_inbox_envelope_with_headers(envelope, None)
            .await
    }

    async fn dispatch_inbox_envelope_with_headers(
        &self,
        envelope: InboxEnvelope,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        if let Some(trigger_id) = envelope.trigger_id {
            let binding = super::registry::resolve_live_trigger_binding(
                &trigger_id,
                envelope.binding_version,
            )
            .map_err(|error| DispatchError::Registry(error.to_string()))?;
            return Ok(vec![
                self.dispatch_with_replay(&binding, envelope.event, None, None, parent_headers)
                    .await?,
            ]);
        }

        let cron_target = match &envelope.event.provider_payload {
            crate::triggers::ProviderPayload::Known(
                crate::triggers::event::KnownProviderPayload::Cron(payload),
            ) => payload.cron_id.clone(),
            _ => None,
        };
        if let Some(trigger_id) = cron_target {
            let binding = super::registry::resolve_live_trigger_binding(
                &trigger_id,
                envelope.binding_version,
            )
            .map_err(|error| DispatchError::Registry(error.to_string()))?;
            return Ok(vec![
                self.dispatch_with_replay(&binding, envelope.event, None, None, parent_headers)
                    .await?,
            ]);
        }

        self.dispatch_event(envelope.event).await
    }

    pub async fn dispatch_event(
        &self,
        event: TriggerEvent,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        let bindings = matching_bindings(&event);
        let mut outcomes = Vec::new();
        for binding in bindings {
            outcomes.push(self.dispatch(&binding, event.clone()).await?);
        }
        Ok(outcomes)
    }

    pub async fn dispatch(
        &self,
        binding: &TriggerBinding,
        event: TriggerEvent,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.dispatch_with_replay(binding, event, None, None, None)
            .await
    }

    pub async fn dispatch_replay(
        &self,
        binding: &TriggerBinding,
        event: TriggerEvent,
        replay_of_event_id: String,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.dispatch_with_replay(binding, event, Some(replay_of_event_id), None, None)
            .await
    }

    pub async fn dispatch_with_parent_span_id(
        &self,
        binding: &TriggerBinding,
        event: TriggerEvent,
        parent_span_id: Option<String>,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.dispatch_with_replay(binding, event, None, parent_span_id, None)
            .await
    }

    async fn dispatch_with_replay(
        &self,
        binding: &TriggerBinding,
        event: TriggerEvent,
        replay_of_event_id: Option<String>,
        parent_span_id: Option<String>,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let span = tracing::info_span!(
            "dispatch",
            trigger_id = %binding.id.as_str(),
            binding_version = binding.version,
            trace_id = %event.trace_id.0
        );
        #[cfg(feature = "otel")]
        let span_for_otel = span.clone();
        let _ = if let Some(headers) = parent_headers {
            crate::observability::otel::set_span_parent_from_headers(
                &span,
                headers,
                &event.trace_id,
                parent_span_id.as_deref(),
            )
        } else {
            crate::observability::otel::set_span_parent(
                &span,
                &event.trace_id,
                parent_span_id.as_deref(),
            )
        };
        #[cfg(feature = "otel")]
        let started_at = Instant::now();
        let metrics = self.metrics.clone();
        let outcome = ACTIVE_DISPATCH_IS_REPLAY
            .scope(
                replay_of_event_id.is_some(),
                self.dispatch_with_replay_inner(binding, event, replay_of_event_id)
                    .instrument(span),
            )
            .await;
        if let Some(metrics) = metrics.as_ref() {
            match &outcome {
                Ok(dispatch_outcome) => match dispatch_outcome.status {
                    DispatchStatus::Succeeded | DispatchStatus::Skipped => {
                        metrics.record_dispatch_succeeded();
                    }
                    DispatchStatus::Waiting => {}
                    _ => metrics.record_dispatch_failed(),
                },
                Err(_) => metrics.record_dispatch_failed(),
            }
            let outcome_label = match &outcome {
                Ok(dispatch_outcome) => dispatch_outcome.status.as_str(),
                Err(DispatchError::Cancelled(_)) => "cancelled",
                Err(_) => "failed",
            };
            metrics.record_trigger_dispatched(
                binding.id.as_str(),
                binding.handler.kind(),
                outcome_label,
            );
            metrics.set_trigger_inflight(binding.id.as_str(), binding.metrics_snapshot().in_flight);
        }
        #[cfg(feature = "otel")]
        {
            use tracing_opentelemetry::OpenTelemetrySpanExt as _;

            let duration_ms = started_at.elapsed().as_millis() as i64;
            let status = match &outcome {
                Ok(dispatch_outcome) => match dispatch_outcome.status {
                    DispatchStatus::Succeeded => "succeeded",
                    DispatchStatus::Skipped => "skipped",
                    DispatchStatus::Waiting => "waiting",
                    DispatchStatus::Cancelled => "cancelled",
                    DispatchStatus::Failed => "failed",
                    DispatchStatus::Dlq => "dlq",
                },
                Err(DispatchError::Cancelled(_)) => "cancelled",
                Err(_) => "failed",
            };
            span_for_otel.set_attribute("result.status", status);
            span_for_otel.set_attribute("result.duration_ms", duration_ms);
        }
        outcome
    }

    async fn dispatch_with_replay_inner(
        &self,
        binding: &TriggerBinding,
        event: TriggerEvent,
        replay_of_event_id: Option<String>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let autonomy_tier = crate::resolve_agent_autonomy_tier(
            &self.event_log,
            binding.id.as_str(),
            binding.autonomy_tier,
        )
        .await
        .unwrap_or(binding.autonomy_tier);
        let binding_key = binding.binding_key();
        let route = DispatchUri::from(&binding.handler);
        let trigger_id = binding.id.as_str().to_string();
        let event_id = event.id.0.clone();
        self.state.in_flight.fetch_add(1, Ordering::Relaxed);
        let begin = if replay_of_event_id.is_some() {
            super::registry::begin_replay_in_flight(binding.id.as_str(), binding.version)
        } else {
            begin_in_flight(binding.id.as_str(), binding.version)
        };
        begin.map_err(|error| DispatchError::Registry(error.to_string()))?;

        let mut attempts = Vec::new();
        let mut source_node_id = format!("trigger:{}", event.id.0);
        let mut initial_nodes = Vec::new();
        let mut initial_edges = Vec::new();
        if let Some(original_event_id) = replay_of_event_id.as_ref() {
            let original_node_id = format!("trigger:{original_event_id}");
            initial_nodes.push(RunActionGraphNodeRecord {
                id: original_node_id.clone(),
                label: format!(
                    "{}:{} (original {})",
                    event.provider.as_str(),
                    event.kind,
                    original_event_id
                ),
                kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
                status: "historical".to_string(),
                outcome: "replayed_from".to_string(),
                trace_id: Some(event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: None,
                run_path: None,
                metadata: trigger_node_metadata(&event),
            });
            initial_edges.push(RunActionGraphEdgeRecord {
                from_id: original_node_id,
                to_id: source_node_id.clone(),
                kind: ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN.to_string(),
                label: Some("replay chain".to_string()),
            });
        }
        initial_nodes.push(RunActionGraphNodeRecord {
            id: source_node_id.clone(),
            label: format!("{}:{}", event.provider.as_str(), event.kind),
            kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
            status: "received".to_string(),
            outcome: "received".to_string(),
            trace_id: Some(event.trace_id.0.clone()),
            stage_id: None,
            node_id: None,
            worker_id: None,
            run_id: None,
            run_path: None,
            metadata: trigger_node_metadata(&event),
        });
        self.emit_action_graph(
            &event,
            initial_nodes,
            initial_edges,
            serde_json::json!({
                "source": "dispatcher",
                "trigger_id": trigger_id,
                "binding_key": binding_key,
                "event_id": event_id,
                "replay_of_event_id": replay_of_event_id,
            }),
        )
        .await?;

        if dispatch_cancel_requested(
            &self.event_log,
            &binding_key,
            &event.id.0,
            replay_of_event_id.as_ref(),
        )
        .await?
        {
            finish_in_flight(
                binding.id.as_str(),
                binding.version,
                TriggerDispatchOutcome::Failed,
            )
            .await
            .map_err(|error| DispatchError::Registry(error.to_string()))?;
            decrement_in_flight(&self.state);
            return Ok(cancelled_dispatch_outcome(
                binding,
                &route,
                &event,
                replay_of_event_id,
                0,
                "trigger cancel request cancelled dispatch before attempt 1".to_string(),
            ));
        }

        if let Some(predicate) = binding.when.as_ref() {
            let predicate_node_id = format!("predicate:{binding_key}:{}", event.id.0);
            let evaluation = self
                .evaluate_predicate(
                    binding,
                    predicate,
                    &event,
                    replay_of_event_id.as_ref(),
                    autonomy_tier,
                )
                .await?;
            let passed = evaluation.result;
            self.emit_action_graph(
                &event,
                vec![RunActionGraphNodeRecord {
                    id: predicate_node_id.clone(),
                    label: predicate.raw.clone(),
                    kind: ACTION_GRAPH_NODE_KIND_TRIGGER_PREDICATE.to_string(),
                    status: "completed".to_string(),
                    outcome: passed.to_string(),
                    trace_id: Some(event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: None,
                    run_path: None,
                    metadata: predicate_node_metadata(binding, predicate, &event, &evaluation),
                }],
                vec![RunActionGraphEdgeRecord {
                    from_id: source_node_id.clone(),
                    to_id: predicate_node_id.clone(),
                    kind: ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH.to_string(),
                    label: None,
                }],
                serde_json::json!({
                    "source": "dispatcher",
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "predicate": predicate.raw,
                    "reason": evaluation.reason,
                    "cached": evaluation.cached,
                    "cost_usd": evaluation.cost_usd,
                    "tokens": evaluation.tokens,
                    "latency_ms": evaluation.latency_ms,
                    "replay_of_event_id": replay_of_event_id,
                }),
            )
            .await?;

            if !passed {
                self.append_skipped_outbox_event(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id.as_ref(),
                    DispatchSkipStage::Predicate,
                    serde_json::json!({
                        "predicate": predicate.raw,
                        "reason": evaluation.reason,
                    }),
                )
                .await?;
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Dispatched,
                )
                .await
                .map_err(|error| DispatchError::Registry(error.to_string()))?;
                decrement_in_flight(&self.state);
                self.append_dispatch_trust_record(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id.as_ref(),
                    autonomy_tier,
                    TrustOutcome::Denied,
                    "skipped",
                    0,
                    None,
                )
                .await?;
                return Ok(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0,
                    attempt_count: 0,
                    status: DispatchStatus::Skipped,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id,
                    result: Some(serde_json::json!({
                        "skipped": true,
                        "predicate": predicate.raw,
                        "reason": evaluation.reason,
                    })),
                    error: None,
                });
            }

            source_node_id = predicate_node_id;
        }

        let (event, acquired_flow) = match self
            .apply_flow_control(binding, &event, replay_of_event_id.as_ref())
            .await?
        {
            FlowControlOutcome::Dispatch { event, acquired } => {
                (*event, Arc::new(AsyncMutex::new(acquired)))
            }
            FlowControlOutcome::Skip { reason } => {
                self.append_skipped_outbox_event(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id.as_ref(),
                    DispatchSkipStage::FlowControl,
                    serde_json::json!({
                        "flow_control": reason,
                    }),
                )
                .await?;
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Dispatched,
                )
                .await
                .map_err(|error| DispatchError::Registry(error.to_string()))?;
                decrement_in_flight(&self.state);
                return Ok(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0,
                    attempt_count: 0,
                    status: DispatchStatus::Skipped,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id,
                    result: Some(serde_json::json!({
                        "skipped": true,
                        "flow_control": reason,
                    })),
                    error: None,
                });
            }
        };

        let mut previous_retry_node = None;
        let max_attempts = binding.retry.max_attempts();
        for attempt in 1..=max_attempts {
            if dispatch_cancel_requested(
                &self.event_log,
                &binding_key,
                &event.id.0,
                replay_of_event_id.as_ref(),
            )
            .await?
            {
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Failed,
                )
                .await
                .map_err(|error| DispatchError::Registry(error.to_string()))?;
                decrement_in_flight(&self.state);
                return Ok(cancelled_dispatch_outcome(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id,
                    attempt.saturating_sub(1),
                    format!("trigger cancel request cancelled dispatch before attempt {attempt}"),
                ));
            }
            maybe_fail_before_outbox();
            let started_at = now_rfc3339();
            let attempt_node_id = dispatch_node_id(&route, &binding_key, &event.id.0, attempt);
            self.append_lifecycle_event(
                "DispatchStarted",
                &event,
                binding,
                serde_json::json!({
                    "event_id": event.id.0,
                    "attempt": attempt,
                    "handler_kind": route.kind(),
                    "target_uri": route.target_uri(),
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id.as_ref(),
            )
            .await?;
            self.append_topic_event(
                TRIGGER_OUTBOX_TOPIC,
                "dispatch_started",
                &event,
                Some(binding),
                Some(attempt),
                serde_json::json!({
                    "event_id": event.id.0,
                    "attempt": attempt,
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "handler_kind": route.kind(),
                    "target_uri": route.target_uri(),
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id.as_ref(),
            )
            .await?;

            let mut dispatch_edges = Vec::new();
            if attempt == 1 {
                dispatch_edges.push(RunActionGraphEdgeRecord {
                    from_id: source_node_id.clone(),
                    to_id: attempt_node_id.clone(),
                    kind: dispatch_entry_edge_kind(&route, binding.when.is_some()).to_string(),
                    label: binding.when.as_ref().map(|_| "true".to_string()),
                });
            } else if let Some(retry_node_id) = previous_retry_node.take() {
                dispatch_edges.push(RunActionGraphEdgeRecord {
                    from_id: retry_node_id,
                    to_id: attempt_node_id.clone(),
                    kind: ACTION_GRAPH_EDGE_KIND_RETRY.to_string(),
                    label: Some(format!("attempt {attempt}")),
                });
            }

            self.emit_action_graph(
                &event,
                vec![RunActionGraphNodeRecord {
                    id: attempt_node_id.clone(),
                    label: dispatch_node_label(&route),
                    kind: dispatch_node_kind(&route).to_string(),
                    status: "running".to_string(),
                    outcome: format!("attempt_{attempt}"),
                    trace_id: Some(event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: None,
                    run_path: None,
                    metadata: dispatch_node_metadata(&route, binding, &event, attempt),
                }],
                dispatch_edges,
                serde_json::json!({
                    "source": "dispatcher",
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "attempt": attempt,
                    "handler_kind": route.kind(),
                    "target_uri": route.target_uri(),
                    "target_agent": dispatch_target_agent(&route),
                    "replay_of_event_id": replay_of_event_id,
                }),
            )
            .await?;

            let result = self
                .dispatch_once(
                    binding,
                    &route,
                    &event,
                    autonomy_tier,
                    Some(DispatchWaitLease::new(
                        self.state.clone(),
                        acquired_flow.clone(),
                    )),
                    &mut self.cancel_tx.subscribe(),
                )
                .await;
            let completed_at = now_rfc3339();

            match result {
                Ok(result) => {
                    let attempt_record = DispatchAttemptRecord {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0.clone(),
                        attempt,
                        handler_kind: route.kind().to_string(),
                        started_at,
                        completed_at,
                        outcome: "success".to_string(),
                        error_msg: None,
                    };
                    attempts.push(attempt_record.clone());
                    self.append_attempt_record(
                        &event,
                        binding,
                        &attempt_record,
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.append_lifecycle_event(
                        "DispatchSucceeded",
                        &event,
                        binding,
                        serde_json::json!({
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "result": result,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.append_topic_event(
                        TRIGGER_OUTBOX_TOPIC,
                        "dispatch_succeeded",
                        &event,
                        Some(binding),
                        Some(attempt),
                        serde_json::json!({
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "trigger_id": binding.id.as_str(),
                            "binding_key": binding.binding_key(),
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "result": result,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.emit_action_graph(
                        &event,
                        vec![RunActionGraphNodeRecord {
                            id: attempt_node_id.clone(),
                            label: dispatch_node_label(&route),
                            kind: dispatch_node_kind(&route).to_string(),
                            status: "completed".to_string(),
                            outcome: dispatch_success_outcome(&route, &result).to_string(),
                            trace_id: Some(event.trace_id.0.clone()),
                            stage_id: None,
                            node_id: None,
                            worker_id: None,
                            run_id: None,
                            run_path: None,
                            metadata: dispatch_success_metadata(
                                &route, binding, &event, attempt, &result,
                            ),
                        }],
                        Vec::new(),
                        serde_json::json!({
                            "source": "dispatcher",
                            "trigger_id": binding.id.as_str(),
                            "binding_key": binding.binding_key(),
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "result": result,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                    )
                    .await?;
                    finish_in_flight(
                        binding.id.as_str(),
                        binding.version,
                        TriggerDispatchOutcome::Dispatched,
                    )
                    .await
                    .map_err(|error| DispatchError::Registry(error.to_string()))?;
                    self.release_flow_control(&acquired_flow).await?;
                    decrement_in_flight(&self.state);
                    self.append_dispatch_trust_record(
                        binding,
                        &route,
                        &event,
                        replay_of_event_id.as_ref(),
                        autonomy_tier,
                        TrustOutcome::Success,
                        "succeeded",
                        attempt,
                        None,
                    )
                    .await?;
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: attempt,
                        status: DispatchStatus::Succeeded,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
                        replay_of_event_id,
                        result: Some(result),
                        error: None,
                    });
                }
                Err(error) => {
                    let attempt_record = DispatchAttemptRecord {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0.clone(),
                        attempt,
                        handler_kind: route.kind().to_string(),
                        started_at,
                        completed_at,
                        outcome: dispatch_error_label(&error).to_string(),
                        error_msg: Some(error.to_string()),
                    };
                    attempts.push(attempt_record.clone());
                    self.append_attempt_record(
                        &event,
                        binding,
                        &attempt_record,
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    if let DispatchError::Waiting(message) = &error {
                        self.append_lifecycle_event(
                            "DispatchWaiting",
                            &event,
                            binding,
                            serde_json::json!({
                                "event_id": event.id.0,
                                "attempt": attempt,
                                "handler_kind": route.kind(),
                                "target_uri": route.target_uri(),
                                "message": message,
                                "replay_of_event_id": replay_of_event_id,
                            }),
                            replay_of_event_id.as_ref(),
                        )
                        .await?;
                        self.append_topic_event(
                            TRIGGER_OUTBOX_TOPIC,
                            "dispatch_waiting",
                            &event,
                            Some(binding),
                            Some(attempt),
                            serde_json::json!({
                                "event_id": event.id.0,
                                "attempt": attempt,
                                "trigger_id": binding.id.as_str(),
                                "binding_key": binding.binding_key(),
                                "handler_kind": route.kind(),
                                "target_uri": route.target_uri(),
                                "message": message,
                                "replay_of_event_id": replay_of_event_id,
                            }),
                            replay_of_event_id.as_ref(),
                        )
                        .await?;
                        finish_in_flight(
                            binding.id.as_str(),
                            binding.version,
                            TriggerDispatchOutcome::Dispatched,
                        )
                        .await
                        .map_err(|registry_error| {
                            DispatchError::Registry(registry_error.to_string())
                        })?;
                        self.release_flow_control(&acquired_flow).await?;
                        decrement_in_flight(&self.state);
                        return Ok(DispatchOutcome {
                            trigger_id: binding.id.as_str().to_string(),
                            binding_key: binding.binding_key(),
                            event_id: event.id.0,
                            attempt_count: attempt,
                            status: DispatchStatus::Waiting,
                            handler_kind: route.kind().to_string(),
                            target_uri: route.target_uri(),
                            replay_of_event_id,
                            result: Some(serde_json::json!({
                                "waiting": true,
                                "message": message,
                            })),
                            error: None,
                        });
                    }

                    self.append_lifecycle_event(
                        "DispatchFailed",
                        &event,
                        binding,
                        serde_json::json!({
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "error": error.to_string(),
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.append_topic_event(
                        TRIGGER_OUTBOX_TOPIC,
                        "dispatch_failed",
                        &event,
                        Some(binding),
                        Some(attempt),
                        serde_json::json!({
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "trigger_id": binding.id.as_str(),
                            "binding_key": binding.binding_key(),
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "error": error.to_string(),
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.emit_action_graph(
                        &event,
                        vec![RunActionGraphNodeRecord {
                            id: attempt_node_id.clone(),
                            label: dispatch_node_label(&route),
                            kind: dispatch_node_kind(&route).to_string(),
                            status: if matches!(error, DispatchError::Cancelled(_)) {
                                "cancelled".to_string()
                            } else {
                                "failed".to_string()
                            },
                            outcome: dispatch_error_label(&error).to_string(),
                            trace_id: Some(event.trace_id.0.clone()),
                            stage_id: None,
                            node_id: None,
                            worker_id: None,
                            run_id: None,
                            run_path: None,
                            metadata: dispatch_error_metadata(
                                &route, binding, &event, attempt, &error,
                            ),
                        }],
                        Vec::new(),
                        serde_json::json!({
                            "source": "dispatcher",
                            "trigger_id": binding.id.as_str(),
                            "binding_key": binding.binding_key(),
                            "event_id": event.id.0,
                            "attempt": attempt,
                            "handler_kind": route.kind(),
                            "target_uri": route.target_uri(),
                            "error": error.to_string(),
                            "replay_of_event_id": replay_of_event_id,
                        }),
                    )
                    .await?;

                    if !error.retryable() {
                        finish_in_flight(
                            binding.id.as_str(),
                            binding.version,
                            TriggerDispatchOutcome::Failed,
                        )
                        .await
                        .map_err(|registry_error| {
                            DispatchError::Registry(registry_error.to_string())
                        })?;
                        self.release_flow_control(&acquired_flow).await?;
                        decrement_in_flight(&self.state);
                        let trust_outcome = match error {
                            DispatchError::Denied(_) => TrustOutcome::Denied,
                            DispatchError::Timeout(_) => TrustOutcome::Timeout,
                            _ => TrustOutcome::Failure,
                        };
                        let terminal_status = if matches!(error, DispatchError::Cancelled(_)) {
                            "cancelled"
                        } else {
                            "failed"
                        };
                        self.append_dispatch_trust_record(
                            binding,
                            &route,
                            &event,
                            replay_of_event_id.as_ref(),
                            autonomy_tier,
                            trust_outcome,
                            terminal_status,
                            attempt,
                            Some(error.to_string()),
                        )
                        .await?;
                        return Ok(DispatchOutcome {
                            trigger_id: binding.id.as_str().to_string(),
                            binding_key: binding.binding_key(),
                            event_id: event.id.0,
                            attempt_count: attempt,
                            status: if matches!(error, DispatchError::Cancelled(_)) {
                                DispatchStatus::Cancelled
                            } else {
                                DispatchStatus::Failed
                            },
                            handler_kind: route.kind().to_string(),
                            target_uri: route.target_uri(),
                            replay_of_event_id,
                            result: None,
                            error: Some(error.to_string()),
                        });
                    }

                    if let Some(delay) = binding.retry.next_retry_delay(attempt) {
                        if let Some(metrics) = self.metrics.as_ref() {
                            metrics.record_retry_scheduled();
                            metrics.record_trigger_retry(binding.id.as_str(), attempt + 1);
                        }
                        let retry_node_id = format!("retry:{binding_key}:{}:{attempt}", event.id.0);
                        previous_retry_node = Some(retry_node_id.clone());
                        self.emit_action_graph(
                            &event,
                            vec![RunActionGraphNodeRecord {
                                id: retry_node_id.clone(),
                                label: format!("retry in {}ms", delay.as_millis()),
                                kind: ACTION_GRAPH_NODE_KIND_RETRY.to_string(),
                                status: "scheduled".to_string(),
                                outcome: format!("attempt_{}", attempt + 1),
                                trace_id: Some(event.trace_id.0.clone()),
                                stage_id: None,
                                node_id: None,
                                worker_id: None,
                                run_id: None,
                                run_path: None,
                                metadata: retry_node_metadata(
                                    binding,
                                    &event,
                                    attempt + 1,
                                    delay,
                                    &error,
                                ),
                            }],
                            vec![RunActionGraphEdgeRecord {
                                from_id: attempt_node_id,
                                to_id: retry_node_id.clone(),
                                kind: ACTION_GRAPH_EDGE_KIND_RETRY.to_string(),
                                label: Some(format!("attempt {}", attempt + 1)),
                            }],
                            serde_json::json!({
                                "source": "dispatcher",
                                "trigger_id": binding.id.as_str(),
                                "binding_key": binding.binding_key(),
                                "event_id": event.id.0,
                                "attempt": attempt + 1,
                                "delay_ms": delay.as_millis(),
                                "replay_of_event_id": replay_of_event_id,
                            }),
                        )
                        .await?;
                        self.append_lifecycle_event(
                            "RetryScheduled",
                            &event,
                            binding,
                            serde_json::json!({
                                "event_id": event.id.0,
                                "attempt": attempt + 1,
                                "delay_ms": delay.as_millis(),
                                "error": error.to_string(),
                                "replay_of_event_id": replay_of_event_id,
                            }),
                            replay_of_event_id.as_ref(),
                        )
                        .await?;
                        self.append_topic_event(
                            TRIGGER_ATTEMPTS_TOPIC,
                            "retry_scheduled",
                            &event,
                            Some(binding),
                            Some(attempt + 1),
                            serde_json::json!({
                                "event_id": event.id.0,
                                "attempt": attempt + 1,
                                "trigger_id": binding.id.as_str(),
                                "binding_key": binding.binding_key(),
                                "delay_ms": delay.as_millis(),
                                "error": error.to_string(),
                                "replay_of_event_id": replay_of_event_id,
                            }),
                            replay_of_event_id.as_ref(),
                        )
                        .await?;
                        self.state.retry_queue_depth.fetch_add(1, Ordering::Relaxed);
                        let sleep_result = sleep_or_cancel_or_request(
                            &self.event_log,
                            delay,
                            &binding_key,
                            &event.id.0,
                            replay_of_event_id.as_ref(),
                            &mut self.cancel_tx.subscribe(),
                        )
                        .await;
                        decrement_retry_queue_depth(&self.state);
                        if sleep_result.is_err() {
                            finish_in_flight(
                                binding.id.as_str(),
                                binding.version,
                                TriggerDispatchOutcome::Failed,
                            )
                            .await
                            .map_err(|registry_error| {
                                DispatchError::Registry(registry_error.to_string())
                            })?;
                            self.release_flow_control(&acquired_flow).await?;
                            decrement_in_flight(&self.state);
                            self.append_dispatch_trust_record(
                                binding,
                                &route,
                                &event,
                                replay_of_event_id.as_ref(),
                                autonomy_tier,
                                TrustOutcome::Failure,
                                "cancelled",
                                attempt,
                                Some("dispatcher shutdown cancelled retry wait".to_string()),
                            )
                            .await?;
                            return Ok(DispatchOutcome {
                                trigger_id: binding.id.as_str().to_string(),
                                binding_key: binding.binding_key(),
                                event_id: event.id.0,
                                attempt_count: attempt,
                                status: DispatchStatus::Cancelled,
                                handler_kind: route.kind().to_string(),
                                target_uri: route.target_uri(),
                                replay_of_event_id,
                                result: None,
                                error: Some("dispatcher shutdown cancelled retry wait".to_string()),
                            });
                        }
                        continue;
                    }

                    let final_error = error.to_string();
                    let dlq_entry = DlqEntry {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event: event.clone(),
                        attempt_count: attempt,
                        final_error: final_error.clone(),
                        attempts: attempts.clone(),
                    };
                    self.state
                        .dlq
                        .lock()
                        .expect("dispatcher dlq poisoned")
                        .push(dlq_entry.clone());
                    if let Some(metrics) = self.metrics.as_ref() {
                        metrics.record_trigger_dlq(binding.id.as_str(), "retry_exhausted");
                    }
                    self.emit_action_graph(
                        &event,
                        vec![RunActionGraphNodeRecord {
                            id: format!("dlq:{binding_key}:{}", event.id.0),
                            label: binding.id.as_str().to_string(),
                            kind: ACTION_GRAPH_NODE_KIND_DLQ.to_string(),
                            status: "queued".to_string(),
                            outcome: "retry_exhausted".to_string(),
                            trace_id: Some(event.trace_id.0.clone()),
                            stage_id: None,
                            node_id: None,
                            worker_id: None,
                            run_id: None,
                            run_path: None,
                            metadata: dlq_node_metadata(binding, &event, attempt, &final_error),
                        }],
                        vec![RunActionGraphEdgeRecord {
                            from_id: dispatch_node_id(&route, &binding_key, &event.id.0, attempt),
                            to_id: format!("dlq:{binding_key}:{}", event.id.0),
                            kind: ACTION_GRAPH_EDGE_KIND_DLQ_MOVE.to_string(),
                            label: Some(format!("{attempt} attempts")),
                        }],
                        serde_json::json!({
                            "source": "dispatcher",
                            "trigger_id": binding.id.as_str(),
                            "binding_key": binding.binding_key(),
                            "event_id": event.id.0,
                            "attempt_count": attempt,
                            "final_error": final_error,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                    )
                    .await?;
                    self.append_lifecycle_event(
                        "DlqMoved",
                        &event,
                        binding,
                        serde_json::json!({
                            "event_id": event.id.0,
                            "attempt_count": attempt,
                            "final_error": dlq_entry.final_error,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    self.append_topic_event(
                        TRIGGER_DLQ_TOPIC,
                        "dlq_moved",
                        &event,
                        Some(binding),
                        Some(attempt),
                        serde_json::to_value(&dlq_entry)
                            .map_err(|serde_error| DispatchError::Serde(serde_error.to_string()))?,
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    finish_in_flight(
                        binding.id.as_str(),
                        binding.version,
                        TriggerDispatchOutcome::Dlq,
                    )
                    .await
                    .map_err(|registry_error| {
                        DispatchError::Registry(registry_error.to_string())
                    })?;
                    self.release_flow_control(&acquired_flow).await?;
                    decrement_in_flight(&self.state);
                    self.append_dispatch_trust_record(
                        binding,
                        &route,
                        &event,
                        replay_of_event_id.as_ref(),
                        autonomy_tier,
                        TrustOutcome::Failure,
                        "dlq",
                        attempt,
                        Some(error.to_string()),
                    )
                    .await?;
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: attempt,
                        status: DispatchStatus::Dlq,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
                        replay_of_event_id,
                        result: None,
                        error: Some(error.to_string()),
                    });
                }
            }
        }

        finish_in_flight(
            binding.id.as_str(),
            binding.version,
            TriggerDispatchOutcome::Failed,
        )
        .await
        .map_err(|error| DispatchError::Registry(error.to_string()))?;
        self.release_flow_control(&acquired_flow).await?;
        decrement_in_flight(&self.state);
        self.append_dispatch_trust_record(
            binding,
            &route,
            &event,
            replay_of_event_id.as_ref(),
            autonomy_tier,
            TrustOutcome::Failure,
            "failed",
            max_attempts,
            Some("dispatch exhausted without terminal outcome".to_string()),
        )
        .await?;
        Ok(DispatchOutcome {
            trigger_id: binding.id.as_str().to_string(),
            binding_key: binding.binding_key(),
            event_id: event.id.0,
            attempt_count: max_attempts,
            status: DispatchStatus::Failed,
            handler_kind: route.kind().to_string(),
            target_uri: route.target_uri(),
            replay_of_event_id,
            result: None,
            error: Some("dispatch exhausted without terminal outcome".to_string()),
        })
    }

    async fn dispatch_once(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<serde_json::Value, DispatchError> {
        match route {
            DispatchUri::Local { .. } => {
                let TriggerHandlerSpec::Local { closure, .. } = &binding.handler else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to a local dispatch URI but does not carry a local closure",
                        binding.id.as_str()
                    )));
                };
                let value = self
                    .invoke_vm_callable(
                        closure,
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &format!("{}.{}", event.provider.as_str(), event.kind),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await?;
                Ok(vm_value_to_json(&value))
            }
            DispatchUri::A2a {
                target,
                allow_cleartext,
            } => {
                if self.state.shutting_down.load(Ordering::SeqCst) {
                    return Err(DispatchError::Cancelled(
                        "dispatcher shutdown cancelled A2A dispatch".to_string(),
                    ));
                }
                let (_endpoint, ack) = crate::a2a::dispatch_trigger_event(
                    target,
                    *allow_cleartext,
                    binding.id.as_str(),
                    &binding.binding_key(),
                    event,
                    cancel_rx,
                )
                .await
                .map_err(|error| match error {
                    crate::a2a::A2aClientError::Cancelled(message) => {
                        DispatchError::Cancelled(message)
                    }
                    other => DispatchError::A2a(other.to_string()),
                })?;
                match ack {
                    crate::a2a::DispatchAck::InlineResult { result, .. } => Ok(result),
                    crate::a2a::DispatchAck::PendingTask { handle, .. } => Ok(handle),
                }
            }
            DispatchUri::Worker { queue } => {
                let receipt = crate::WorkerQueue::new(self.event_log.clone())
                    .enqueue(&crate::WorkerQueueJob {
                        queue: queue.clone(),
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        binding_version: binding.version,
                        event: event.clone(),
                        replay_of_event_id: current_dispatch_context()
                            .and_then(|context| context.replay_of_event_id),
                        priority: worker_queue_priority(binding, event),
                    })
                    .await
                    .map_err(DispatchError::from)?;
                Ok(serde_json::to_value(receipt)
                    .map_err(|error| DispatchError::Serde(error.to_string()))?)
            }
        }
    }

    async fn apply_flow_control(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
    ) -> Result<FlowControlOutcome, DispatchError> {
        let flow = &binding.flow_control;
        let mut managed_event = event.clone();

        if let Some(batch) = &flow.batch {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    batch.key.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            match self
                .state
                .flow_control
                .consume_batch(&gate, batch.size, batch.timeout, managed_event.clone())
                .await
                .map_err(DispatchError::from)?
            {
                BatchDecision::Dispatch(events) => {
                    managed_event = build_batched_event(events)?;
                }
                BatchDecision::Merged => {
                    return Ok(FlowControlOutcome::Skip {
                        reason: "batch_merged".to_string(),
                    })
                }
            }
        }

        if let Some(debounce) = &flow.debounce {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    Some(&debounce.key),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            let latest = self
                .state
                .flow_control
                .debounce(&gate, debounce.period)
                .await
                .map_err(DispatchError::from)?;
            if !latest {
                return Ok(FlowControlOutcome::Skip {
                    reason: "debounced".to_string(),
                });
            }
        }

        if let Some(rate_limit) = &flow.rate_limit {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    rate_limit.key.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            let allowed = self
                .state
                .flow_control
                .check_rate_limit(&gate, rate_limit.period, rate_limit.max)
                .await
                .map_err(DispatchError::from)?;
            if !allowed {
                return Ok(FlowControlOutcome::Skip {
                    reason: "rate_limited".to_string(),
                });
            }
        }

        if let Some(throttle) = &flow.throttle {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    throttle.key.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            self.state
                .flow_control
                .wait_for_throttle(&gate, throttle.period, throttle.max)
                .await
                .map_err(DispatchError::from)?;
        }

        let mut acquired = AcquiredFlowControl::default();
        if let Some(singleton) = &flow.singleton {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    singleton.key.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            let acquired_singleton = self
                .state
                .flow_control
                .try_acquire_singleton(&gate)
                .await
                .map_err(DispatchError::from)?;
            if !acquired_singleton {
                return Ok(FlowControlOutcome::Skip {
                    reason: "singleton_active".to_string(),
                });
            }
            acquired.singleton = Some(SingletonLease { gate, held: true });
        }

        if let Some(concurrency) = &flow.concurrency {
            let gate = self
                .resolve_flow_gate(
                    &binding.binding_key(),
                    concurrency.key.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            let priority_rank = self
                .resolve_priority_rank(
                    &binding.binding_key(),
                    flow.priority.as_ref(),
                    &managed_event,
                    replay_of_event_id,
                )
                .await?;
            let permit = self
                .state
                .flow_control
                .acquire_concurrency(&gate, concurrency.max, priority_rank)
                .await
                .map_err(DispatchError::from)?;
            acquired.concurrency = Some(ConcurrencyLease {
                gate,
                max: concurrency.max,
                priority_rank,
                permit: Some(permit),
            });
        }

        Ok(FlowControlOutcome::Dispatch {
            event: Box::new(managed_event),
            acquired,
        })
    }

    async fn release_flow_control(
        &self,
        acquired: &Arc<AsyncMutex<AcquiredFlowControl>>,
    ) -> Result<(), DispatchError> {
        let (singleton_gate, concurrency_permit) = {
            let mut acquired = acquired.lock().await;
            let singleton_gate = acquired.singleton.as_mut().and_then(|lease| {
                if lease.held {
                    lease.held = false;
                    Some(lease.gate.clone())
                } else {
                    None
                }
            });
            let concurrency_permit = acquired
                .concurrency
                .as_mut()
                .and_then(|lease| lease.permit.take());
            (singleton_gate, concurrency_permit)
        };
        if let Some(gate) = singleton_gate {
            self.state
                .flow_control
                .release_singleton(&gate)
                .await
                .map_err(DispatchError::from)?;
        }
        if let Some(permit) = concurrency_permit {
            self.state
                .flow_control
                .release_concurrency(permit)
                .await
                .map_err(DispatchError::from)?;
        }
        Ok(())
    }

    async fn resolve_flow_gate(
        &self,
        binding_key: &str,
        expr: Option<&crate::triggers::TriggerExpressionSpec>,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
    ) -> Result<String, DispatchError> {
        let key = match expr {
            Some(expr) => {
                self.evaluate_flow_expression(binding_key, expr, event, replay_of_event_id)
                    .await?
            }
            None => "_global".to_string(),
        };
        Ok(format!("{binding_key}:{key}"))
    }

    async fn resolve_priority_rank(
        &self,
        binding_key: &str,
        priority: Option<&crate::triggers::TriggerPriorityOrderConfig>,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
    ) -> Result<usize, DispatchError> {
        let Some(priority) = priority else {
            return Ok(0);
        };
        let value = self
            .evaluate_flow_expression(binding_key, &priority.key, event, replay_of_event_id)
            .await?;
        Ok(priority
            .order
            .iter()
            .position(|candidate| candidate == &value)
            .unwrap_or(priority.order.len()))
    }

    async fn evaluate_flow_expression(
        &self,
        binding_key: &str,
        expr: &crate::triggers::TriggerExpressionSpec,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
    ) -> Result<String, DispatchError> {
        let value = self
            .invoke_vm_callable(
                &expr.closure,
                binding_key,
                event,
                replay_of_event_id,
                "",
                "flow_control",
                AutonomyTier::Suggest,
                None,
                &mut self.cancel_tx.subscribe(),
            )
            .await?;
        Ok(json_value_to_gate(&vm_value_to_json(&value)))
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke_vm_callable(
        &self,
        closure: &crate::value::VmClosure,
        binding_key: &str,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        agent_id: &str,
        action: &str,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<VmValue, DispatchError> {
        let mut vm = self.base_vm.child_vm();
        let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if self.state.shutting_down.load(Ordering::SeqCst) {
            cancel_token.store(true, Ordering::SeqCst);
        }
        self.state
            .cancel_tokens
            .lock()
            .expect("dispatcher cancel tokens poisoned")
            .push(cancel_token.clone());
        vm.install_cancel_token(cancel_token.clone());
        let arg = event_to_handler_value(event)?;
        let args = [arg];
        let future = vm.call_closure_pub(closure, &args);
        pin_mut!(future);
        let (binding_id, binding_version) = split_binding_key(binding_key);
        let prior_context = ACTIVE_DISPATCH_CONTEXT.with(|slot| {
            slot.borrow_mut().replace(DispatchContext {
                trigger_event: event.clone(),
                replay_of_event_id: replay_of_event_id.cloned(),
                binding_id,
                binding_version,
                agent_id: agent_id.to_string(),
                action: action.to_string(),
                autonomy_tier,
            })
        });
        let prior_wait_lease = ACTIVE_DISPATCH_WAIT_LEASE
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), wait_lease));
        let prior_hitl_state = crate::stdlib::hitl::take_hitl_state();
        crate::stdlib::hitl::reset_hitl_state();
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                result = &mut future => break result,
                _ = recv_cancel(cancel_rx) => {
                    cancel_token.store(true, Ordering::SeqCst);
                }
                _ = poll.tick() => {
                    if dispatch_cancel_requested(
                        &self.event_log,
                        binding_key,
                        &event.id.0,
                        replay_of_event_id,
                    )
                    .await? {
                        cancel_token.store(true, Ordering::SeqCst);
                    }
                }
            }
        };
        ACTIVE_DISPATCH_CONTEXT.with(|slot| {
            *slot.borrow_mut() = prior_context;
        });
        ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| {
            *slot.borrow_mut() = prior_wait_lease;
        });
        crate::stdlib::hitl::restore_hitl_state(prior_hitl_state);
        {
            let mut tokens = self
                .state
                .cancel_tokens
                .lock()
                .expect("dispatcher cancel tokens poisoned");
            tokens.retain(|token| !Arc::ptr_eq(token, &cancel_token));
        }

        if cancel_token.load(Ordering::SeqCst) {
            if dispatch_cancel_requested(
                &self.event_log,
                binding_key,
                &event.id.0,
                replay_of_event_id,
            )
            .await?
            {
                Err(DispatchError::Cancelled(
                    "trigger cancel request cancelled local handler".to_string(),
                ))
            } else {
                Err(DispatchError::Cancelled(
                    "dispatcher shutdown cancelled local handler".to_string(),
                ))
            }
        } else {
            result.map_err(dispatch_error_from_vm_error)
        }
    }

    async fn evaluate_predicate(
        &self,
        binding: &TriggerBinding,
        predicate: &super::registry::TriggerPredicateSpec,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        autonomy_tier: AutonomyTier,
    ) -> Result<PredicateEvaluationRecord, DispatchError> {
        let event_id = event.id.0.clone();
        let trigger_id = binding.id.as_str().to_string();
        let now_ms = now_unix_ms();
        let today = utc_day_key();

        let breaker_open_until = {
            let mut state = binding
                .predicate_state
                .lock()
                .expect("trigger predicate state poisoned");
            if state.budget_day_utc != Some(today) {
                state.budget_day_utc = Some(today);
                binding
                    .metrics
                    .cost_today_usd_micros
                    .store(0, Ordering::Relaxed);
            }
            if state
                .breaker_open_until_ms
                .is_some_and(|until_ms| until_ms > now_ms)
            {
                state.breaker_open_until_ms
            } else {
                None
            }
        };

        if breaker_open_until.is_some() {
            let mut metadata = BTreeMap::new();
            metadata.insert("trigger_id".to_string(), serde_json::json!(trigger_id));
            metadata.insert("event_id".to_string(), serde_json::json!(event_id));
            metadata.insert(
                "breaker_open_until_ms".to_string(),
                serde_json::json!(breaker_open_until),
            );
            crate::events::log_warn_meta(
                "trigger.predicate.circuit_breaker",
                "trigger predicate circuit breaker is open; short-circuiting to false",
                metadata,
            );
            let record = PredicateEvaluationRecord {
                result: false,
                reason: Some("circuit_open".to_string()),
                ..Default::default()
            };
            self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
                .await?;
            return Ok(record);
        }

        if binding
            .daily_cost_usd
            .is_some_and(|limit| current_predicate_daily_cost(binding) > limit)
        {
            self.append_lifecycle_event(
                "predicate.daily_budget_exceeded",
                event,
                binding,
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "event_id": event.id.0,
                    "limit_usd": binding.daily_cost_usd,
                    "cost_today_usd": current_predicate_daily_cost(binding),
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id,
            )
            .await?;
            let record = PredicateEvaluationRecord {
                result: false,
                reason: Some("daily_budget_exceeded".to_string()),
                ..Default::default()
            };
            self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
                .await?;
            return Ok(record);
        }

        let replay_cache = self
            .read_predicate_cache_record(replay_of_event_id.unwrap_or(&event_id))
            .await?;
        let guard = start_predicate_evaluation(
            binding.when_budget.clone().unwrap_or_default(),
            replay_cache,
        );
        let started = std::time::Instant::now();
        let eval = self
            .invoke_vm_callable_with_timeout(
                &predicate.closure,
                &binding.binding_key(),
                event,
                replay_of_event_id,
                binding.id.as_str(),
                &format!("{}.{}", event.provider.as_str(), event.kind),
                autonomy_tier,
                &mut self.cancel_tx.subscribe(),
                binding
                    .when_budget
                    .as_ref()
                    .and_then(|budget| budget.timeout()),
            )
            .await;
        let capture = guard.finish();
        let latency_ms = started.elapsed().as_millis() as u64;
        if replay_of_event_id.is_none() && !capture.entries.is_empty() {
            self.append_predicate_cache_record(binding, event, &capture.entries)
                .await?;
        }

        let mut record = PredicateEvaluationRecord {
            result: false,
            cost_usd: capture.total_cost_usd,
            tokens: capture.total_tokens,
            latency_ms,
            cached: capture.cached,
            reason: None,
        };

        let mut count_failure = false;
        let mut opened_breaker = false;

        match eval {
            Ok(value) => match predicate_value_as_bool(value) {
                Ok(result) => {
                    record.result = result;
                }
                Err(reason) => {
                    count_failure = true;
                    record.reason = Some(reason);
                }
            },
            Err(error) => {
                count_failure = true;
                record.reason = Some(error.to_string());
            }
        }

        let cost_usd_micros = usd_to_micros(record.cost_usd);
        if cost_usd_micros > 0 {
            binding
                .metrics
                .cost_total_usd_micros
                .fetch_add(cost_usd_micros, Ordering::Relaxed);
            binding
                .metrics
                .cost_today_usd_micros
                .fetch_add(cost_usd_micros, Ordering::Relaxed);
        }

        let timed_out = matches!(
            record.reason.as_deref(),
            Some("predicate evaluation timed out")
        );
        if capture.budget_exceeded || timed_out {
            record.result = false;
            record.reason = Some("budget_exceeded".to_string());
            self.append_lifecycle_event(
                "predicate.budget_exceeded",
                event,
                binding,
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "event_id": event.id.0,
                    "max_cost_usd": binding.when_budget.as_ref().and_then(|budget| budget.max_cost_usd),
                    "tokens_max": binding.when_budget.as_ref().and_then(|budget| budget.tokens_max),
                    "cost_usd": record.cost_usd,
                    "tokens": record.tokens,
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id,
            )
            .await?;
        }

        if binding
            .daily_cost_usd
            .is_some_and(|limit| current_predicate_daily_cost(binding) > limit)
        {
            record.result = false;
            record.reason = Some("daily_budget_exceeded".to_string());
            self.append_lifecycle_event(
                "predicate.daily_budget_exceeded",
                event,
                binding,
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "event_id": event.id.0,
                    "limit_usd": binding.daily_cost_usd,
                    "cost_today_usd": current_predicate_daily_cost(binding),
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id,
            )
            .await?;
        }

        {
            let mut state = binding
                .predicate_state
                .lock()
                .expect("trigger predicate state poisoned");
            if state.budget_day_utc != Some(today) {
                state.budget_day_utc = Some(today);
                binding
                    .metrics
                    .cost_today_usd_micros
                    .store(cost_usd_micros, Ordering::Relaxed);
            }
            if count_failure {
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                if state.consecutive_failures >= 3 {
                    state.breaker_open_until_ms = Some(now_ms.saturating_add(5 * 60 * 1000));
                    opened_breaker = true;
                }
            } else {
                state.consecutive_failures = 0;
                state.breaker_open_until_ms = None;
            }
        }

        if opened_breaker {
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "trigger_id".to_string(),
                serde_json::json!(binding.id.as_str()),
            );
            metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
            metadata.insert("failure_count".to_string(), serde_json::json!(3));
            metadata.insert("reason".to_string(), serde_json::json!(record.reason));
            crate::events::log_warn_meta(
                "trigger.predicate.circuit_breaker",
                "trigger predicate circuit breaker opened for 5 minutes",
                metadata,
            );
        }

        self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
            .await?;
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    async fn invoke_vm_callable_with_timeout(
        &self,
        closure: &crate::value::VmClosure,
        binding_key: &str,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        agent_id: &str,
        action: &str,
        autonomy_tier: AutonomyTier,
        cancel_rx: &mut broadcast::Receiver<()>,
        timeout: Option<Duration>,
    ) -> Result<VmValue, DispatchError> {
        let future = self.invoke_vm_callable(
            closure,
            binding_key,
            event,
            replay_of_event_id,
            agent_id,
            action,
            autonomy_tier,
            None,
            cancel_rx,
        );
        pin_mut!(future);
        if let Some(timeout) = timeout {
            match tokio::time::timeout(timeout, future).await {
                Ok(result) => result,
                Err(_) => Err(DispatchError::Local(
                    "predicate evaluation timed out".to_string(),
                )),
            }
        } else {
            future.await
        }
    }

    async fn append_predicate_evaluated_event(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        record: &PredicateEvaluationRecord,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.record_trigger_predicate_evaluation(
                binding.id.as_str(),
                record.result,
                record.cost_usd,
            );
            metrics.set_trigger_budget_cost_today(
                binding.id.as_str(),
                current_predicate_daily_cost(binding),
            );
            if matches!(
                record.reason.as_deref(),
                Some("budget_exceeded" | "daily_budget_exceeded")
            ) {
                metrics.record_trigger_budget_exhausted(
                    binding.id.as_str(),
                    record.reason.as_deref().unwrap_or("predicate"),
                );
            }
        }
        self.append_lifecycle_event(
            "predicate.evaluated",
            event,
            binding,
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "event_id": event.id.0,
                "result": record.result,
                "cost_usd": record.cost_usd,
                "tokens": record.tokens,
                "latency_ms": record.latency_ms,
                "cached": record.cached,
                "reason": record.reason,
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await
    }

    async fn append_predicate_cache_record(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        entries: &[PredicateCacheEntry],
    ) -> Result<(), DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_LEGACY_TOPIC)
            .expect("static trigger inbox legacy topic name is valid");
        let payload = serde_json::to_value(PredicateCacheRecord {
            trigger_id: binding.id.as_str().to_string(),
            event_id: event.id.0.clone(),
            entries: entries.to_vec(),
        })
        .map_err(|error| DispatchError::Serde(error.to_string()))?;
        self.event_log
            .append(&topic, LogEvent::new("predicate_llm_cache", payload))
            .await
            .map_err(DispatchError::from)
            .map(|_| ())
    }

    async fn read_predicate_cache_record(
        &self,
        event_id: &str,
    ) -> Result<Vec<PredicateCacheEntry>, DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_LEGACY_TOPIC)
            .expect("static trigger inbox legacy topic name is valid");
        let records = self
            .event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .map_err(DispatchError::from)?;
        Ok(records
            .into_iter()
            .filter(|(_, event)| event.kind == "predicate_llm_cache")
            .filter_map(|(_, event)| {
                serde_json::from_value::<PredicateCacheRecord>(event.payload).ok()
            })
            .filter(|record| record.event_id == event_id)
            .flat_map(|record| record.entries)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn append_dispatch_trust_record(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        autonomy_tier: AutonomyTier,
        outcome: TrustOutcome,
        terminal_status: &str,
        attempt_count: u32,
        error: Option<String>,
    ) -> Result<(), DispatchError> {
        let mut record = TrustRecord::new(
            binding.id.as_str().to_string(),
            format!("{}.{}", event.provider.as_str(), event.kind),
            None,
            outcome,
            event.trace_id.0.clone(),
            autonomy_tier,
        );
        record.metadata.insert(
            "binding_key".to_string(),
            serde_json::json!(binding.binding_key()),
        );
        record.metadata.insert(
            "binding_version".to_string(),
            serde_json::json!(binding.version),
        );
        record.metadata.insert(
            "provider".to_string(),
            serde_json::json!(event.provider.as_str()),
        );
        record
            .metadata
            .insert("event_kind".to_string(), serde_json::json!(event.kind));
        record
            .metadata
            .insert("handler_kind".to_string(), serde_json::json!(route.kind()));
        record.metadata.insert(
            "target_uri".to_string(),
            serde_json::json!(route.target_uri()),
        );
        record.metadata.insert(
            "terminal_status".to_string(),
            serde_json::json!(terminal_status),
        );
        record.metadata.insert(
            "attempt_count".to_string(),
            serde_json::json!(attempt_count),
        );
        if let Some(replay_of_event_id) = replay_of_event_id {
            record.metadata.insert(
                "replay_of_event_id".to_string(),
                serde_json::json!(replay_of_event_id),
            );
        }
        if let Some(error) = error {
            record
                .metadata
                .insert("error".to_string(), serde_json::json!(error));
        }
        append_trust_record(&self.event_log, &record)
            .await
            .map(|_| ())
            .map_err(DispatchError::from)
    }

    async fn append_attempt_record(
        &self,
        event: &TriggerEvent,
        binding: &TriggerBinding,
        attempt: &DispatchAttemptRecord,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_ATTEMPTS_TOPIC,
            "attempt_recorded",
            event,
            Some(binding),
            Some(attempt.attempt),
            serde_json::to_value(attempt)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
            replay_of_event_id,
        )
        .await
    }

    async fn append_lifecycle_event(
        &self,
        kind: &str,
        event: &TriggerEvent,
        binding: &TriggerBinding,
        payload: serde_json::Value,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGERS_LIFECYCLE_TOPIC,
            kind,
            event,
            Some(binding),
            None,
            payload,
            replay_of_event_id,
        )
        .await
    }

    async fn append_skipped_outbox_event(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        stage: DispatchSkipStage,
        detail: serde_json::Value,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_OUTBOX_TOPIC,
            "dispatch_skipped",
            event,
            Some(binding),
            None,
            serde_json::json!({
                "event_id": event.id.0,
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "handler_kind": route.kind(),
                "target_uri": route.target_uri(),
                "skip_stage": stage.as_str(),
                "detail": detail,
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await
    }

    async fn append_topic_event(
        &self,
        topic_name: &str,
        kind: &str,
        event: &TriggerEvent,
        binding: Option<&TriggerBinding>,
        attempt: Option<u32>,
        payload: serde_json::Value,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        let topic = Topic::new(topic_name)
            .expect("static trigger dispatcher topic names should always be valid");
        let headers = event_headers(event, binding, attempt, replay_of_event_id);
        self.event_log
            .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
            .await
            .map_err(DispatchError::from)
            .map(|_| ())
    }

    async fn emit_action_graph(
        &self,
        event: &TriggerEvent,
        nodes: Vec<RunActionGraphNodeRecord>,
        edges: Vec<RunActionGraphEdgeRecord>,
        extra: serde_json::Value,
    ) -> Result<(), DispatchError> {
        let mut headers = BTreeMap::new();
        headers.insert("trace_id".to_string(), event.trace_id.0.clone());
        headers.insert("event_id".to_string(), event.id.0.clone());
        let observability = RunObservabilityRecord {
            schema_version: 1,
            action_graph_nodes: nodes,
            action_graph_edges: edges,
            ..Default::default()
        };
        append_action_graph_update(
            headers,
            serde_json::json!({
                "source": "dispatcher",
                "trace_id": event.trace_id.0,
                "event_id": event.id.0,
                "observability": observability,
                "context": extra,
            }),
        )
        .await
        .map_err(DispatchError::from)
    }
}

async fn dispatch_cancel_requested(
    event_log: &Arc<AnyEventLog>,
    binding_key: &str,
    event_id: &str,
    replay_of_event_id: Option<&String>,
) -> Result<bool, DispatchError> {
    if replay_of_event_id.is_some() {
        return Ok(false);
    }
    let topic = Topic::new(TRIGGER_CANCEL_REQUESTS_TOPIC)
        .expect("static trigger cancel topic should always be valid");
    let events = event_log.read_range(&topic, None, usize::MAX).await?;
    let requested = events
        .into_iter()
        .filter(|(_, event)| event.kind == "dispatch_cancel_requested")
        .filter_map(|(_, event)| {
            serde_json::from_value::<DispatchCancelRequest>(event.payload).ok()
        })
        .collect::<BTreeSet<_>>();
    Ok(requested
        .iter()
        .any(|request| request.binding_key == binding_key && request.event_id == event_id))
}

async fn sleep_or_cancel_or_request(
    event_log: &Arc<AnyEventLog>,
    delay: Duration,
    binding_key: &str,
    event_id: &str,
    replay_of_event_id: Option<&String>,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    let sleep = tokio::time::sleep(delay);
    pin_mut!(sleep);
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = &mut sleep => return Ok(()),
            _ = recv_cancel(cancel_rx) => {
                return Err(DispatchError::Cancelled(
                    "dispatcher shutdown cancelled retry wait".to_string(),
                ));
            }
            _ = poll.tick() => {
                if dispatch_cancel_requested(event_log, binding_key, event_id, replay_of_event_id).await? {
                    return Err(DispatchError::Cancelled(
                        "trigger cancel request cancelled retry wait".to_string(),
                    ));
                }
            }
        }
    }
}

fn build_batched_event(events: Vec<TriggerEvent>) -> Result<TriggerEvent, DispatchError> {
    let mut iter = events.into_iter();
    let Some(mut root) = iter.next() else {
        return Err(DispatchError::Registry(
            "batch dispatch produced an empty event list".to_string(),
        ));
    };
    let mut batch = Vec::new();
    batch.push(
        serde_json::to_value(&root).map_err(|error| DispatchError::Serde(error.to_string()))?,
    );
    for event in iter {
        batch.push(
            serde_json::to_value(&event)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        );
    }
    root.batch = Some(batch);
    Ok(root)
}

fn json_value_to_gate(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "unserializable".to_string()),
    }
}

fn event_to_handler_value(event: &TriggerEvent) -> Result<VmValue, DispatchError> {
    let json =
        serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
    let value = json_to_vm_value(&json);
    match (&event.raw_body, value) {
        (Some(raw_body), VmValue::Dict(dict)) => {
            let mut map = (*dict).clone();
            map.insert(
                "raw_body".to_string(),
                VmValue::Bytes(Rc::new(raw_body.clone())),
            );
            Ok(VmValue::Dict(Rc::new(map)))
        }
        (_, other) => Ok(other),
    }
}

fn decrement_in_flight(state: &DispatcherRuntimeState) {
    let previous = state.in_flight.fetch_sub(1, Ordering::Relaxed);
    if previous == 1 && state.retry_queue_depth.load(Ordering::Relaxed) == 0 {
        state.idle_notify.notify_waiters();
    }
}

fn decrement_retry_queue_depth(state: &DispatcherRuntimeState) {
    let previous = state.retry_queue_depth.fetch_sub(1, Ordering::Relaxed);
    if previous == 1 && state.in_flight.load(Ordering::Relaxed) == 0 {
        state.idle_notify.notify_waiters();
    }
}

#[cfg(test)]
fn install_test_inbox_dequeued_signal(tx: tokio::sync::oneshot::Sender<()>) {
    TEST_INBOX_DEQUEUED_SIGNAL.with(|slot| {
        *slot.borrow_mut() = Some(tx);
    });
}

#[cfg(not(test))]
fn notify_test_inbox_dequeued() {}

#[cfg(test)]
fn notify_test_inbox_dequeued() {
    TEST_INBOX_DEQUEUED_SIGNAL.with(|slot| {
        if let Some(tx) = slot.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
}

pub async fn enqueue_trigger_event<L: EventLog + ?Sized>(
    event_log: &L,
    event: &TriggerEvent,
) -> Result<u64, DispatchError> {
    let topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC)
        .expect("static trigger.inbox.envelopes topic is valid");
    let headers = event_headers(event, None, None, None);
    let payload =
        serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
    event_log
        .append(
            &topic,
            LogEvent::new("event_ingested", payload).with_headers(headers),
        )
        .await
        .map_err(DispatchError::from)
}

pub fn snapshot_dispatcher_stats() -> DispatcherStatsSnapshot {
    ACTIVE_DISPATCHER_STATE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| DispatcherStatsSnapshot {
                in_flight: state.in_flight.load(Ordering::Relaxed),
                retry_queue_depth: state.retry_queue_depth.load(Ordering::Relaxed),
                dlq_depth: state.dlq.lock().expect("dispatcher dlq poisoned").len() as u64,
            })
            .unwrap_or_default()
    })
}

pub fn clear_dispatcher_state() {
    ACTIVE_DISPATCHER_STATE.with(|slot| {
        *slot.borrow_mut() = None;
    });
    ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

fn dispatch_error_from_vm_error(error: VmError) -> DispatchError {
    if let Some(wait_id) = crate::stdlib::waitpoint::is_waitpoint_suspension(&error) {
        return DispatchError::Waiting(format!("waitpoint suspended: {wait_id}"));
    }
    if is_cancelled_vm_error(&error) {
        return DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string());
    }
    if let VmError::Thrown(VmValue::String(message)) = &error {
        return DispatchError::Local(message.to_string());
    }
    match error_to_category(&error) {
        ErrorCategory::Timeout => DispatchError::Timeout(error.to_string()),
        ErrorCategory::ToolRejected => DispatchError::Denied(error.to_string()),
        ErrorCategory::Cancelled => {
            DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string())
        }
        _ => DispatchError::Local(error.to_string()),
    }
}

fn dispatch_error_label(error: &DispatchError) -> &'static str {
    match error {
        DispatchError::Denied(_) => "denied",
        DispatchError::Timeout(_) => "timeout",
        DispatchError::Waiting(_) => "waiting",
        DispatchError::Cancelled(_) => "cancelled",
        _ => "failed",
    }
}

fn dispatch_success_outcome(route: &DispatchUri, result: &serde_json::Value) -> &'static str {
    match route {
        DispatchUri::Worker { .. } => "enqueued",
        DispatchUri::A2a { .. }
            if result.get("kind").and_then(|value| value.as_str()) == Some("a2a_task_handle") =>
        {
            "pending"
        }
        DispatchUri::A2a { .. } => "completed",
        DispatchUri::Local { .. } => "success",
    }
}

fn dispatch_node_id(
    route: &DispatchUri,
    binding_key: &str,
    event_id: &str,
    attempt: u32,
) -> String {
    let prefix = match route {
        DispatchUri::A2a { .. } => "a2a",
        _ => "dispatch",
    };
    format!("{prefix}:{binding_key}:{event_id}:{attempt}")
}

fn dispatch_node_kind(route: &DispatchUri) -> &'static str {
    match route {
        DispatchUri::A2a { .. } => ACTION_GRAPH_NODE_KIND_A2A_HOP,
        DispatchUri::Worker { .. } => ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE,
        _ => ACTION_GRAPH_NODE_KIND_DISPATCH,
    }
}

fn dispatch_node_label(route: &DispatchUri) -> String {
    match route {
        DispatchUri::A2a { target, .. } => crate::a2a::target_agent_label(target),
        _ => route.target_uri(),
    }
}

fn dispatch_target_agent(route: &DispatchUri) -> Option<String> {
    match route {
        DispatchUri::A2a { target, .. } => Some(crate::a2a::target_agent_label(target)),
        _ => None,
    }
}

fn dispatch_entry_edge_kind(route: &DispatchUri, has_predicate: bool) -> &'static str {
    match route {
        DispatchUri::A2a { .. } => ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH,
        _ if has_predicate => ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE,
        _ => ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    }
}

fn signature_status_label(status: &crate::triggers::SignatureStatus) -> &'static str {
    match status {
        crate::triggers::SignatureStatus::Verified => "verified",
        crate::triggers::SignatureStatus::Unsigned => "unsigned",
        crate::triggers::SignatureStatus::Failed { .. } => "failed",
    }
}

fn trigger_node_metadata(event: &TriggerEvent) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "provider".to_string(),
        serde_json::json!(event.provider.as_str()),
    );
    metadata.insert("event_kind".to_string(), serde_json::json!(event.kind));
    metadata.insert(
        "dedupe_key".to_string(),
        serde_json::json!(event.dedupe_key),
    );
    metadata.insert(
        "signature_status".to_string(),
        serde_json::json!(signature_status_label(&event.signature_status)),
    );
    metadata
}

fn predicate_node_metadata(
    binding: &TriggerBinding,
    predicate: &super::registry::TriggerPredicateSpec,
    event: &TriggerEvent,
    evaluation: &PredicateEvaluationRecord,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("predicate".to_string(), serde_json::json!(predicate.raw));
    metadata.insert("result".to_string(), serde_json::json!(evaluation.result));
    metadata.insert(
        "cost_usd".to_string(),
        serde_json::json!(evaluation.cost_usd),
    );
    metadata.insert("tokens".to_string(), serde_json::json!(evaluation.tokens));
    metadata.insert(
        "latency_ms".to_string(),
        serde_json::json!(evaluation.latency_ms),
    );
    metadata.insert("cached".to_string(), serde_json::json!(evaluation.cached));
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    if let Some(reason) = evaluation.reason.as_ref() {
        metadata.insert("reason".to_string(), serde_json::json!(reason));
    }
    metadata
}

fn dispatch_node_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert("handler_kind".to_string(), serde_json::json!(route.kind()));
    metadata.insert(
        "target_uri".to_string(),
        serde_json::json!(route.target_uri()),
    );
    metadata.insert("attempt".to_string(), serde_json::json!(attempt));
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    if let Some(target_agent) = dispatch_target_agent(route) {
        metadata.insert("target_agent".to_string(), serde_json::json!(target_agent));
    }
    if let DispatchUri::Worker { queue } = route {
        metadata.insert("queue_name".to_string(), serde_json::json!(queue));
    }
    metadata
}

fn dispatch_success_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    result: &serde_json::Value,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = dispatch_node_metadata(route, binding, event, attempt);
    match route {
        DispatchUri::A2a { .. } => {
            if let Some(task_id) = result
                .get("task_id")
                .or_else(|| result.get("id"))
                .and_then(|value| value.as_str())
            {
                metadata.insert("task_id".to_string(), serde_json::json!(task_id));
            }
            if let Some(state) = result.get("state").and_then(|value| value.as_str()) {
                metadata.insert("state".to_string(), serde_json::json!(state));
            }
        }
        DispatchUri::Worker { .. } => {
            if let Some(job_event_id) = result.get("job_event_id").and_then(|value| value.as_u64())
            {
                metadata.insert("job_event_id".to_string(), serde_json::json!(job_event_id));
            }
            if let Some(response_topic) = result
                .get("response_topic")
                .and_then(|value| value.as_str())
            {
                metadata.insert(
                    "response_topic".to_string(),
                    serde_json::json!(response_topic),
                );
            }
        }
        DispatchUri::Local { .. } => {}
    }
    metadata
}

fn dispatch_error_metadata(
    route: &DispatchUri,
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    error: &DispatchError,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = dispatch_node_metadata(route, binding, event, attempt);
    metadata.insert("error".to_string(), serde_json::json!(error.to_string()));
    metadata
}

fn retry_node_metadata(
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt: u32,
    delay: Duration,
    error: &DispatchError,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    metadata.insert("attempt".to_string(), serde_json::json!(attempt));
    metadata.insert("delay_ms".to_string(), serde_json::json!(delay.as_millis()));
    metadata.insert("error".to_string(), serde_json::json!(error.to_string()));
    metadata
}

fn dlq_node_metadata(
    binding: &TriggerBinding,
    event: &TriggerEvent,
    attempt_count: u32,
    final_error: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    metadata.insert(
        "attempt_count".to_string(),
        serde_json::json!(attempt_count),
    );
    metadata.insert("final_error".to_string(), serde_json::json!(final_error));
    metadata
}

fn predicate_value_as_bool(value: VmValue) -> Result<bool, String> {
    match value {
        VmValue::Bool(result) => Ok(result),
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok" => match fields.first() {
            Some(VmValue::Bool(result)) => Ok(*result),
            Some(other) => Err(format!(
                "predicate Result.Ok payload must be bool, got {}",
                other.type_name()
            )),
            None => Err("predicate Result.Ok payload is missing".to_string()),
        },
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => Err(fields
            .first()
            .map(VmValue::display)
            .unwrap_or_else(|| "predicate returned Result.Err".to_string())),
        other => Err(format!(
            "predicate must return bool or Result<bool, _>, got {}",
            other.type_name()
        )),
    }
}

fn usd_to_micros(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (value * 1_000_000.0).round() as u64
}

fn current_predicate_daily_cost(binding: &TriggerBinding) -> f64 {
    binding
        .metrics
        .cost_today_usd_micros
        .load(Ordering::Relaxed) as f64
        / 1_000_000.0
}

fn split_binding_key(binding_key: &str) -> (String, u32) {
    let Some((binding_id, suffix)) = binding_key.rsplit_once("@v") else {
        return (binding_key.to_string(), 0);
    };
    let version = suffix.parse::<u32>().unwrap_or(0);
    (binding_id.to_string(), version)
}

fn is_cancelled_vm_error(error: &VmError) -> bool {
    matches!(
        error,
        VmError::Thrown(VmValue::String(message))
            if message.starts_with("kind:cancelled:")
    ) || matches!(error_to_category(error), ErrorCategory::Cancelled)
}

fn event_headers(
    event: &TriggerEvent,
    binding: Option<&TriggerBinding>,
    attempt: Option<u32>,
    replay_of_event_id: Option<&String>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("event_id".to_string(), event.id.0.clone());
    headers.insert("trace_id".to_string(), event.trace_id.0.clone());
    headers.insert("provider".to_string(), event.provider.as_str().to_string());
    headers.insert("kind".to_string(), event.kind.clone());
    if let Some(replay_of_event_id) = replay_of_event_id {
        headers.insert("replay_of_event_id".to_string(), replay_of_event_id.clone());
    }
    if let Some(binding) = binding {
        headers.insert("trigger_id".to_string(), binding.id.as_str().to_string());
        headers.insert("binding_key".to_string(), binding.binding_key());
        headers.insert(
            "handler_kind".to_string(),
            DispatchUri::from(&binding.handler).kind().to_string(),
        );
    }
    if let Some(attempt) = attempt {
        headers.insert("attempt".to_string(), attempt.to_string());
    }
    headers
}

fn worker_queue_priority(
    binding: &super::registry::TriggerBinding,
    event: &TriggerEvent,
) -> crate::WorkerQueuePriority {
    match event
        .headers
        .get("priority")
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("high") => crate::WorkerQueuePriority::High,
        Some("low") => crate::WorkerQueuePriority::Low,
        _ => binding.dispatch_priority,
    }
}

const TEST_FAIL_BEFORE_OUTBOX_ENV: &str = "HARN_TEST_DISPATCHER_FAIL_BEFORE_OUTBOX";

fn maybe_fail_before_outbox() {
    if std::env::var_os(TEST_FAIL_BEFORE_OUTBOX_ENV).is_some() {
        std::process::exit(86);
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn now_unix_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn utc_day_key() -> i32 {
    time::OffsetDateTime::now_utc().date().to_julian_day()
}

fn cancelled_dispatch_outcome(
    binding: &TriggerBinding,
    route: &DispatchUri,
    event: &TriggerEvent,
    replay_of_event_id: Option<String>,
    attempt_count: u32,
    error: String,
) -> DispatchOutcome {
    DispatchOutcome {
        trigger_id: binding.id.as_str().to_string(),
        binding_key: binding.binding_key(),
        event_id: event.id.0.clone(),
        attempt_count,
        status: DispatchStatus::Cancelled,
        handler_kind: route.kind().to_string(),
        target_uri: route.target_uri(),
        replay_of_event_id,
        result: None,
        error: Some(error),
    }
}

async fn recv_cancel(cancel_rx: &mut broadcast::Receiver<()>) {
    let _ = cancel_rx.recv().await;
}

#[cfg(test)]
mod tests;
