use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::orchestration::{
    RunActionGraphEdgeRecord, RunActionGraphNodeRecord, ACTION_GRAPH_EDGE_KIND_DLQ_MOVE,
    ACTION_GRAPH_NODE_KIND_DLQ,
};
use crate::triggers::registry::TriggerBinding;

use super::super::TRIGGER_DLQ_TOPIC;
use super::uri::DispatchUri;
use super::{
    accepted_at_ms, current_unix_ms, duration_between_ms, tenant_id, DispatchError, Dispatcher,
    DlqEntry, TriggerEvent,
};

pub(super) const DESTINATION_CIRCUIT_FAILURE_THRESHOLD: u32 = 5;
const DESTINATION_CIRCUIT_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DestinationCircuitProbe {
    Allow { half_open: bool },
    Block { retry_after: Duration },
}

#[derive(Debug)]
pub(super) struct DestinationCircuitRegistry {
    threshold: u32,
    backoff: Duration,
    states: Mutex<BTreeMap<String, DestinationCircuitState>>,
}

#[derive(Clone, Debug)]
struct DestinationCircuitState {
    failures: u32,
    opened_at: Option<Instant>,
}

impl Default for DestinationCircuitRegistry {
    fn default() -> Self {
        Self {
            threshold: DESTINATION_CIRCUIT_FAILURE_THRESHOLD,
            backoff: DESTINATION_CIRCUIT_BACKOFF,
            states: Mutex::new(BTreeMap::new()),
        }
    }
}

impl DestinationCircuitRegistry {
    pub(super) fn check(&self, destination: &str) -> DestinationCircuitProbe {
        let mut states = self
            .states
            .lock()
            .expect("destination circuit registry poisoned");
        let Some(state) = states.get_mut(destination) else {
            return DestinationCircuitProbe::Allow { half_open: false };
        };
        let Some(opened_at) = state.opened_at else {
            return DestinationCircuitProbe::Allow { half_open: false };
        };
        let elapsed = opened_at.elapsed();
        if elapsed >= self.backoff {
            DestinationCircuitProbe::Allow { half_open: true }
        } else {
            DestinationCircuitProbe::Block {
                retry_after: self.backoff.saturating_sub(elapsed),
            }
        }
    }

    pub(super) fn record_success(&self, destination: &str) {
        let mut states = self
            .states
            .lock()
            .expect("destination circuit registry poisoned");
        states.remove(destination);
    }

    pub(super) fn record_failure(&self, destination: &str) -> bool {
        let mut states = self
            .states
            .lock()
            .expect("destination circuit registry poisoned");
        let state = states
            .entry(destination.to_string())
            .or_insert(DestinationCircuitState {
                failures: 0,
                opened_at: None,
            });
        if state.opened_at.is_some() {
            state.failures = self.threshold;
            state.opened_at = Some(Instant::now());
            return true;
        }
        state.failures = state.failures.saturating_add(1);
        if state.failures >= self.threshold {
            state.opened_at = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

impl Dispatcher {
    pub(super) async fn move_budget_exhausted_to_dlq(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        final_error: &str,
    ) -> Result<(), DispatchError> {
        let dlq_entry = DlqEntry {
            trigger_id: binding.id.as_str().to_string(),
            binding_key: binding.binding_key(),
            event: event.clone(),
            attempt_count: 0,
            final_error: final_error.to_string(),
            error_class: crate::triggers::classify_trigger_dlq_error(final_error).to_string(),
            attempts: Vec::new(),
        };
        self.state
            .dlq
            .lock()
            .expect("dispatcher dlq poisoned")
            .push(dlq_entry.clone());
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.record_trigger_dlq(binding.id.as_str(), "budget_exhausted");
            metrics.record_trigger_accepted_to_dlq(
                binding.id.as_str(),
                &binding.binding_key(),
                event.provider.as_str(),
                tenant_id(event),
                "budget_exhausted",
                duration_between_ms(current_unix_ms(), accepted_at_ms(None, event)),
            );
        }
        tracing::info!(
            component = "dispatcher",
            lifecycle = "dlq_moved",
            trigger_id = %binding.id.as_str(),
            binding_key = %binding.binding_key(),
            event_id = %event.id.0,
            reason = "budget_exhausted",
            trace_id = %event.trace_id.0
        );
        self.emit_action_graph(
            event,
            vec![RunActionGraphNodeRecord {
                id: format!("dlq:{}:{}", binding.binding_key(), event.id.0),
                label: binding.id.as_str().to_string(),
                kind: ACTION_GRAPH_NODE_KIND_DLQ.to_string(),
                status: "queued".to_string(),
                outcome: "budget_exhausted".to_string(),
                trace_id: Some(event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: None,
                run_path: None,
                metadata: dlq_node_metadata(binding, event, 0, final_error),
            }],
            vec![RunActionGraphEdgeRecord {
                from_id: format!("predicate:{}:{}", binding.binding_key(), event.id.0),
                to_id: format!("dlq:{}:{}", binding.binding_key(), event.id.0),
                kind: ACTION_GRAPH_EDGE_KIND_DLQ_MOVE.to_string(),
                label: Some("budget exhausted".to_string()),
            }],
            serde_json::json!({
                "source": "dispatcher",
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "handler_kind": route.kind(),
                "target_uri": route.target_uri(),
                "final_error": final_error,
                "replay_of_event_id": replay_of_event_id,
            }),
        )
        .await?;
        self.append_lifecycle_event(
            "DlqMoved",
            event,
            binding,
            serde_json::json!({
                "event_id": event.id.0,
                "attempt_count": 0,
                "final_error": final_error,
                "reason": "budget_exhausted",
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await?;
        self.append_topic_event(
            TRIGGER_DLQ_TOPIC,
            "dlq_moved",
            event,
            Some(binding),
            None,
            serde_json::to_value(&dlq_entry)
                .map_err(|serde_error| DispatchError::Serde(serde_error.to_string()))?,
            replay_of_event_id,
        )
        .await
    }

    pub(super) async fn move_circuit_open_to_dlq(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        final_error: &str,
        destination: &str,
    ) -> Result<(), DispatchError> {
        let dlq_entry = DlqEntry {
            trigger_id: binding.id.as_str().to_string(),
            binding_key: binding.binding_key(),
            event: event.clone(),
            attempt_count: 0,
            final_error: final_error.to_string(),
            error_class: crate::triggers::classify_trigger_dlq_error(final_error).to_string(),
            attempts: Vec::new(),
        };
        self.state
            .dlq
            .lock()
            .expect("dispatcher dlq poisoned")
            .push(dlq_entry.clone());
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.record_trigger_dlq(binding.id.as_str(), "circuit_open");
            metrics.record_trigger_accepted_to_dlq(
                binding.id.as_str(),
                &binding.binding_key(),
                event.provider.as_str(),
                tenant_id(event),
                "circuit_open",
                Duration::ZERO,
            );
        }
        tracing::info!(
            component = "dispatcher",
            lifecycle = "dlq_moved",
            trigger_id = %binding.id.as_str(),
            binding_key = %binding.binding_key(),
            event_id = %event.id.0,
            reason = "circuit_open",
            destination,
            trace_id = %event.trace_id.0
        );
        self.emit_action_graph(
            event,
            vec![RunActionGraphNodeRecord {
                id: format!("dlq:{}:{}", binding.binding_key(), event.id.0),
                label: binding.id.as_str().to_string(),
                kind: ACTION_GRAPH_NODE_KIND_DLQ.to_string(),
                status: "queued".to_string(),
                outcome: "circuit_open".to_string(),
                trace_id: Some(event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: None,
                run_path: None,
                metadata: dlq_node_metadata(binding, event, 0, final_error),
            }],
            vec![RunActionGraphEdgeRecord {
                from_id: format!("trigger:{}", event.id.0),
                to_id: format!("dlq:{}:{}", binding.binding_key(), event.id.0),
                kind: ACTION_GRAPH_EDGE_KIND_DLQ_MOVE.to_string(),
                label: Some("circuit open".to_string()),
            }],
            serde_json::json!({
                "source": "dispatcher",
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "handler_kind": route.kind(),
                "target_uri": route.target_uri(),
                "destination": destination,
                "final_error": final_error,
                "replay_of_event_id": replay_of_event_id,
            }),
        )
        .await?;
        self.append_lifecycle_event(
            "DlqMoved",
            event,
            binding,
            serde_json::json!({
                "event_id": event.id.0,
                "attempt_count": 0,
                "final_error": final_error,
                "reason": "circuit_open",
                "destination": destination,
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await?;
        self.append_topic_event(
            TRIGGER_DLQ_TOPIC,
            "dlq_moved",
            event,
            Some(binding),
            None,
            serde_json::to_value(&dlq_entry)
                .map_err(|serde_error| DispatchError::Serde(serde_error.to_string()))?,
            replay_of_event_id,
        )
        .await
    }
}

pub(super) fn destination_circuit_key(route: &DispatchUri) -> String {
    format!("{}:{}", route.kind(), route.target_uri())
}

pub(super) fn dlq_node_metadata(
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
    metadata.insert(
        "error_class".to_string(),
        serde_json::json!(crate::triggers::classify_trigger_dlq_error(final_error)),
    );
    metadata
}
