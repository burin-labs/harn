use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::event_log::{AnyEventLog, LogError};
use crate::trust_graph::AutonomyTier;
use crate::vm::Vm;

use super::circuits::DestinationCircuitRegistry;
use super::flow_control::{ConcurrencyPermit, FlowControlManager};
use super::TriggerEvent;

pub(super) const DEFAULT_AUTONOMY_BUDGET_REVIEWER: &str = "operator";

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

pub(super) struct DispatchExecutionPolicyGuard;

impl Drop for DispatchExecutionPolicyGuard {
    fn drop(&mut self) {
        crate::orchestration::pop_execution_policy();
    }
}

#[derive(Clone)]
pub struct Dispatcher {
    pub(super) base_vm: Rc<Vm>,
    pub(super) event_log: Arc<AnyEventLog>,
    pub(super) cancel_tx: broadcast::Sender<()>,
    pub(super) state: Arc<DispatcherRuntimeState>,
    pub(super) metrics: Option<Arc<crate::MetricsRegistry>>,
    pub(super) a2a_client: Arc<dyn crate::a2a::A2aClient>,
}

#[derive(Debug)]
pub(super) struct DispatcherRuntimeState {
    pub(super) in_flight: AtomicU64,
    pub(super) retry_queue_depth: AtomicU64,
    pub(super) dlq: Mutex<Vec<DlqEntry>>,
    pub(super) cancel_tokens: Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>,
    pub(super) shutting_down: std::sync::atomic::AtomicBool,
    pub(super) idle_notify: Notify,
    pub(super) flow_control: FlowControlManager,
    pub(super) destination_circuits: DestinationCircuitRegistry,
}

impl DispatcherRuntimeState {
    pub(super) fn new(event_log: Arc<AnyEventLog>) -> Self {
        Self {
            in_flight: AtomicU64::new(0),
            retry_queue_depth: AtomicU64::new(0),
            dlq: Mutex::new(Vec::new()),
            cancel_tokens: Mutex::new(Vec::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            idle_notify: Notify::new(),
            flow_control: FlowControlManager::new(event_log),
            destination_circuits: DestinationCircuitRegistry::default(),
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
    #[serde(default = "default_dlq_error_class")]
    pub error_class: String,
    pub attempts: Vec<DispatchAttemptRecord>,
}

fn default_dlq_error_class() -> String {
    "unknown".to_string()
}

#[derive(Clone, Debug)]
pub(super) struct SingletonLease {
    pub(super) gate: String,
    pub(super) held: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ConcurrencyLease {
    pub(super) gate: String,
    pub(super) max: u32,
    pub(super) priority_rank: usize,
    pub(super) permit: Option<ConcurrencyPermit>,
}

#[derive(Default, Debug)]
pub(super) struct AcquiredFlowControl {
    pub(super) singleton: Option<SingletonLease>,
    pub(super) concurrency: Option<ConcurrencyLease>,
}

#[derive(Clone)]
pub(crate) struct DispatchWaitLease {
    state: Arc<DispatcherRuntimeState>,
    acquired: Arc<AsyncMutex<AcquiredFlowControl>>,
    suspended: Arc<AtomicBool>,
}

impl DispatchWaitLease {
    pub(super) fn new(
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

pub(super) enum FlowControlOutcome {
    Dispatch {
        event: Box<TriggerEvent>,
        acquired: AcquiredFlowControl,
    },
    Skip {
        reason: String,
    },
}

#[derive(Debug)]
pub(super) struct DispatchCallResult {
    pub(super) output: serde_json::Value,
    pub(super) metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug)]
pub(super) enum DispatchSkipStage {
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
    pub(super) fn retryable(&self) -> bool {
        !matches!(
            self,
            Self::Cancelled(_) | Self::Denied(_) | Self::NotImplemented(_) | Self::Waiting(_)
        )
    }
}

impl DispatchSkipStage {
    pub(super) fn as_str(&self) -> &'static str {
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
