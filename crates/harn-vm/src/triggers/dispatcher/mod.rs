use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{pin_mut, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogError, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::orchestration::{
    append_action_graph_update, RunActionGraphEdgeRecord, RunActionGraphNodeRecord,
    RunObservabilityRecord, ACTION_GRAPH_EDGE_KIND_DLQ_MOVE, ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE,
    ACTION_GRAPH_EDGE_KIND_RETRY, ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    ACTION_GRAPH_NODE_KIND_DISPATCH, ACTION_GRAPH_NODE_KIND_DLQ, ACTION_GRAPH_NODE_KIND_RETRY,
    ACTION_GRAPH_NODE_KIND_TRIGGER,
};
use crate::stdlib::json_to_vm_value;
use crate::value::{error_to_category, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use self::uri::DispatchUri;
use super::registry::matching_bindings;
use super::registry::{TriggerBinding, TriggerHandlerSpec};
use super::{begin_in_flight, finish_in_flight, TriggerDispatchOutcome, TriggerEvent};

pub mod retry;
pub mod uri;

pub use retry::{RetryPolicy, TriggerRetryConfig, DEFAULT_MAX_ATTEMPTS};

const TRIGGER_INBOX_TOPIC: &str = "trigger.inbox";
const TRIGGER_OUTBOX_TOPIC: &str = "trigger.outbox";
const TRIGGER_ATTEMPTS_TOPIC: &str = "trigger.attempts";
const TRIGGER_DLQ_TOPIC: &str = "trigger.dlq";
const TRIGGERS_LIFECYCLE_TOPIC: &str = "triggers.lifecycle";

thread_local! {
    static ACTIVE_DISPATCHER_STATE: RefCell<Option<Arc<DispatcherRuntimeState>>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub struct Dispatcher {
    base_vm: Rc<Vm>,
    event_log: Arc<AnyEventLog>,
    cancel_tx: broadcast::Sender<()>,
    state: Arc<DispatcherRuntimeState>,
}

#[derive(Debug)]
struct DispatcherRuntimeState {
    in_flight: AtomicU64,
    retry_queue_depth: AtomicU64,
    dlq: Mutex<Vec<DlqEntry>>,
    cancel_tokens: Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>,
    shutting_down: std::sync::atomic::AtomicBool,
}

impl Default for DispatcherRuntimeState {
    fn default() -> Self {
        Self {
            in_flight: AtomicU64::new(0),
            retry_queue_depth: AtomicU64::new(0),
            dlq: Mutex::new(Vec::new()),
            cancel_tokens: Mutex::new(Vec::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
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
    Cancelled,
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
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DlqEntry {
    pub trigger_id: String,
    pub binding_key: String,
    pub event: TriggerEvent,
    pub attempt_count: u32,
    pub final_error: String,
    pub attempts: Vec<DispatchAttemptRecord>,
}

#[derive(Debug)]
pub enum DispatchError {
    EventLog(String),
    Registry(String),
    Serde(String),
    Local(String),
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
            | Self::Cancelled(message)
            | Self::NotImplemented(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DispatchError {}

impl DispatchError {
    fn retryable(&self) -> bool {
        !matches!(self, Self::Cancelled(_) | Self::NotImplemented(_))
    }
}

impl From<LogError> for DispatchError {
    fn from(value: LogError) -> Self {
        Self::EventLog(value.to_string())
    }
}

impl Dispatcher {
    pub fn new(base_vm: Vm) -> Result<Self, DispatchError> {
        let event_log = active_event_log().ok_or_else(|| {
            DispatchError::EventLog("dispatcher requires an active event log".to_string())
        })?;
        Ok(Self::with_event_log(base_vm, event_log))
    }

    pub fn with_event_log(base_vm: Vm, event_log: Arc<AnyEventLog>) -> Self {
        let state = Arc::new(DispatcherRuntimeState::default());
        ACTIVE_DISPATCHER_STATE.with(|slot| {
            *slot.borrow_mut() = Some(state.clone());
        });
        let (cancel_tx, _) = broadcast::channel(32);
        Self {
            base_vm: Rc::new(base_vm),
            event_log,
            cancel_tx,
            state,
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
        let topic = Topic::new(TRIGGER_INBOX_TOPIC).expect("static trigger.inbox topic is valid");
        let headers = event_headers(&event, None, None);
        let payload = serde_json::to_value(&event)
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
        let topic = Topic::new(TRIGGER_INBOX_TOPIC).expect("static trigger.inbox topic is valid");
        let stream = self.event_log.clone().subscribe(&topic, None).await?;
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
                    let trigger_event: TriggerEvent = serde_json::from_value(event.payload)
                        .map_err(|error| DispatchError::Serde(error.to_string()))?;
                    let dispatcher = self.clone();
                    tokio::task::spawn_local(async move {
                        let _ = dispatcher.dispatch_event(trigger_event).await;
                    });
                }
                _ = recv_cancel(&mut cancel_rx) => break,
            }
        }

        Ok(())
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
        let binding_key = binding.binding_key();
        let route = DispatchUri::from(&binding.handler);
        let trigger_id = binding.id.as_str().to_string();
        let event_id = event.id.0.clone();
        self.state.in_flight.fetch_add(1, Ordering::Relaxed);
        begin_in_flight(binding.id.as_str(), binding.version)
            .map_err(|error| DispatchError::Registry(error.to_string()))?;

        let mut attempts = Vec::new();
        let mut source_node_id = format!("trigger:{}", event.id.0);
        self.emit_action_graph(
            &event,
            vec![RunActionGraphNodeRecord {
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
            }],
            Vec::new(),
            serde_json::json!({
                "source": "dispatcher",
                "trigger_id": trigger_id,
                "binding_key": binding_key,
                "event_id": event_id,
            }),
        )
        .await?;

        if let Some(predicate) = binding.when.as_ref() {
            let predicate_node_id = format!("predicate:{binding_key}:{}", event.id.0);
            let predicate_result = self
                .invoke_vm_callable(&predicate.closure, &event, &mut self.cancel_tx.subscribe())
                .await?;
            let passed = matches!(predicate_result, VmValue::Bool(true));
            self.emit_action_graph(
                &event,
                vec![RunActionGraphNodeRecord {
                    id: predicate_node_id.clone(),
                    label: predicate.raw.clone(),
                    kind: crate::orchestration::ACTION_GRAPH_NODE_KIND_PREDICATE.to_string(),
                    status: "completed".to_string(),
                    outcome: passed.to_string(),
                    trace_id: Some(event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: None,
                    run_path: None,
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
                }),
            )
            .await?;

            if !passed {
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Dispatched,
                )
                .await
                .map_err(|error| DispatchError::Registry(error.to_string()))?;
                self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
                return Ok(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0,
                    attempt_count: 0,
                    status: DispatchStatus::Skipped,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    result: None,
                    error: None,
                });
            }

            source_node_id = predicate_node_id;
        }

        let mut previous_retry_node = None;
        let max_attempts = binding.retry.max_attempts();
        for attempt in 1..=max_attempts {
            let started_at = now_rfc3339();
            let dispatch_node_id = format!("dispatch:{binding_key}:{}:{attempt}", event.id.0);
            self.append_lifecycle_event(
                "DispatchStarted",
                &event,
                binding,
                serde_json::json!({
                    "event_id": event.id.0,
                    "attempt": attempt,
                    "handler_kind": route.kind(),
                    "target_uri": route.target_uri(),
                }),
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
                }),
            )
            .await?;

            let mut dispatch_edges = Vec::new();
            if attempt == 1 {
                dispatch_edges.push(RunActionGraphEdgeRecord {
                    from_id: source_node_id.clone(),
                    to_id: dispatch_node_id.clone(),
                    kind: if binding.when.is_some() {
                        ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE.to_string()
                    } else {
                        ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH.to_string()
                    },
                    label: binding.when.as_ref().map(|_| "true".to_string()),
                });
            } else if let Some(retry_node_id) = previous_retry_node.take() {
                dispatch_edges.push(RunActionGraphEdgeRecord {
                    from_id: retry_node_id,
                    to_id: dispatch_node_id.clone(),
                    kind: ACTION_GRAPH_EDGE_KIND_RETRY.to_string(),
                    label: Some(format!("attempt {attempt}")),
                });
            }

            self.emit_action_graph(
                &event,
                vec![RunActionGraphNodeRecord {
                    id: dispatch_node_id.clone(),
                    label: route.target_uri(),
                    kind: ACTION_GRAPH_NODE_KIND_DISPATCH.to_string(),
                    status: "running".to_string(),
                    outcome: format!("attempt_{attempt}"),
                    trace_id: Some(event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: None,
                    run_path: None,
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
                }),
            )
            .await?;

            let result = self
                .dispatch_once(binding, &route, &event, &mut self.cancel_tx.subscribe())
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
                    self.append_attempt_record(&event, binding, &attempt_record)
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
                        }),
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
                    self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: attempt,
                        status: DispatchStatus::Succeeded,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
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
                        outcome: if matches!(error, DispatchError::Cancelled(_)) {
                            "cancelled".to_string()
                        } else {
                            "failed".to_string()
                        },
                        error_msg: Some(error.to_string()),
                    };
                    attempts.push(attempt_record.clone());
                    self.append_attempt_record(&event, binding, &attempt_record)
                        .await?;
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
                        }),
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
                        self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
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
                            result: None,
                            error: Some(error.to_string()),
                        });
                    }

                    if let Some(delay) = binding.retry.next_retry_delay(attempt) {
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
                            }],
                            vec![RunActionGraphEdgeRecord {
                                from_id: dispatch_node_id,
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
                            }),
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
                            }),
                        )
                        .await?;
                        self.state.retry_queue_depth.fetch_add(1, Ordering::Relaxed);
                        let sleep_result =
                            sleep_or_cancel(delay, &mut self.cancel_tx.subscribe()).await;
                        self.state.retry_queue_depth.fetch_sub(1, Ordering::Relaxed);
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
                            self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
                            return Ok(DispatchOutcome {
                                trigger_id: binding.id.as_str().to_string(),
                                binding_key: binding.binding_key(),
                                event_id: event.id.0,
                                attempt_count: attempt,
                                status: DispatchStatus::Cancelled,
                                handler_kind: route.kind().to_string(),
                                target_uri: route.target_uri(),
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
                        }],
                        vec![RunActionGraphEdgeRecord {
                            from_id: format!("dispatch:{binding_key}:{}:{attempt}", event.id.0),
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
                        }),
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
                    self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: attempt,
                        status: DispatchStatus::Dlq,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
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
        self.state.in_flight.fetch_sub(1, Ordering::Relaxed);
        Ok(DispatchOutcome {
            trigger_id: binding.id.as_str().to_string(),
            binding_key: binding.binding_key(),
            event_id: event.id.0,
            attempt_count: max_attempts,
            status: DispatchStatus::Failed,
            handler_kind: route.kind().to_string(),
            target_uri: route.target_uri(),
            result: None,
            error: Some("dispatch exhausted without terminal outcome".to_string()),
        })
    }

    async fn dispatch_once(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
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
                let value = self.invoke_vm_callable(closure, event, cancel_rx).await?;
                Ok(vm_value_to_json(&value))
            }
            DispatchUri::A2a { target } => Err(DispatchError::NotImplemented(format!(
                "a2a:// dispatch to '{target}' is not implemented yet; see O-04 #181"
            ))),
            DispatchUri::Worker { queue } => Err(DispatchError::NotImplemented(format!(
                "worker:// dispatch to '{queue}' is not implemented yet; see O-05 #182"
            ))),
        }
    }

    async fn invoke_vm_callable(
        &self,
        closure: &crate::value::VmClosure,
        event: &TriggerEvent,
        _cancel_rx: &mut broadcast::Receiver<()>,
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
        let arg = json_to_vm_value(
            &serde_json::to_value(event)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        );
        let args = [arg];
        let future = vm.call_closure_pub(closure, &args, &[]);
        pin_mut!(future);
        let result = future.await;
        let mut tokens = self
            .state
            .cancel_tokens
            .lock()
            .expect("dispatcher cancel tokens poisoned");
        tokens.retain(|token| !Arc::ptr_eq(token, &cancel_token));
        if cancel_token.load(Ordering::SeqCst) {
            Err(DispatchError::Cancelled(
                "dispatcher shutdown cancelled local handler".to_string(),
            ))
        } else {
            result.map_err(dispatch_error_from_vm_error)
        }
    }

    async fn append_attempt_record(
        &self,
        event: &TriggerEvent,
        binding: &TriggerBinding,
        attempt: &DispatchAttemptRecord,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_ATTEMPTS_TOPIC,
            "attempt_recorded",
            event,
            Some(binding),
            Some(attempt.attempt),
            serde_json::to_value(attempt)
                .map_err(|error| DispatchError::Serde(error.to_string()))?,
        )
        .await
    }

    async fn append_lifecycle_event(
        &self,
        kind: &str,
        event: &TriggerEvent,
        binding: &TriggerBinding,
        payload: serde_json::Value,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGERS_LIFECYCLE_TOPIC,
            kind,
            event,
            Some(binding),
            None,
            payload,
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
    ) -> Result<(), DispatchError> {
        let topic = Topic::new(topic_name)
            .expect("static trigger dispatcher topic names should always be valid");
        let headers = event_headers(event, binding, attempt);
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
}

fn dispatch_error_from_vm_error(error: VmError) -> DispatchError {
    if is_cancelled_vm_error(&error) {
        return DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string());
    }
    if let VmError::Thrown(VmValue::String(message)) = &error {
        return DispatchError::Local(message.to_string());
    }
    match error_to_category(&error) {
        ErrorCategory::Cancelled => {
            DispatchError::Cancelled("dispatcher shutdown cancelled local handler".to_string())
        }
        _ => DispatchError::Local(error.to_string()),
    }
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
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("event_id".to_string(), event.id.0.clone());
    headers.insert("trace_id".to_string(), event.trace_id.0.clone());
    headers.insert("provider".to_string(), event.provider.as_str().to_string());
    headers.insert("kind".to_string(), event.kind.clone());
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

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

async fn sleep_or_cancel(
    duration: Duration,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    if duration.is_zero() {
        return Ok(());
    }
    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = recv_cancel(cancel_rx) => Err(DispatchError::Cancelled("dispatcher shutdown cancelled retry wait".to_string())),
    }
}

async fn recv_cancel(cancel_rx: &mut broadcast::Receiver<()>) {
    let _ = cancel_rx.recv().await;
}

#[cfg(test)]
mod tests;
