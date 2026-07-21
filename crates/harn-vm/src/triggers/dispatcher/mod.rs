//! Trigger dispatcher: routes inbox envelopes to bindings, applies retry/flow control,
//! and emits trust + lifecycle events.
//!
//! Most support code lives in dedicated submodules (`types`, `state`, `util`, `audit`,
//! `action_graph`, `vm_invoke`, plus the pre-existing `circuits`, `flow_control`,
//! `predicate_eval`, `retry`, `uri`). The single `impl Dispatcher { ... }` block in this
//! file is intentionally kept whole — splitting it across multiple files would scatter
//! the dispatch state machine across modules without a natural seam.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{pin_mut, StreamExt};
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tracing::Instrument as _;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::orchestration::{
    RunActionGraphEdgeRecord, RunActionGraphNodeRecord, ACTION_GRAPH_EDGE_KIND_DLQ_MOVE,
    ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN, ACTION_GRAPH_EDGE_KIND_RETRY,
    ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH, ACTION_GRAPH_NODE_KIND_DLQ,
    ACTION_GRAPH_NODE_KIND_PREDICATE, ACTION_GRAPH_NODE_KIND_RETRY, ACTION_GRAPH_NODE_KIND_TRIGGER,
};
use crate::trust_graph::{AutonomyTier, TrustOutcome};
use crate::vm::Vm;

use self::uri::DispatchUri;
use super::registry::{
    binding_autonomy_budget_would_exceed, binding_budget_would_exceed, matching_bindings,
    micros_to_usd, note_autonomous_decision, note_binding_budget_cost,
    note_orchestrator_budget_cost, orchestrator_budget_would_exceed, AgentScope, TargetExpr,
    TriggerBinding, TriggerBudgetExhaustionStrategy, TriggerHandlerSpec,
};
use super::{
    begin_in_flight, finish_in_flight, TriggerDispatchOutcome, TriggerEvent,
    TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_CANCEL_REQUESTS_TOPIC,
    TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_OBSERVABILITY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};
use circuits::{
    destination_circuit_key, dlq_node_metadata, DestinationCircuitProbe,
    DESTINATION_CIRCUIT_FAILURE_THRESHOLD,
};
use flow_control::BatchDecision;
use predicate_eval::predicate_node_metadata;

mod action_graph;
mod audit;
mod circuits;
mod flow_control;
mod persona;
mod predicate_eval;
pub mod retry;
pub(crate) mod state;
mod types;
pub mod uri;
mod util;
mod vm_invoke;

pub use retry::{RetryPolicy, TriggerRetryConfig, DEFAULT_MAX_ATTEMPTS};
pub use types::{
    DispatchAttemptRecord, DispatchCancelRequest, DispatchError, DispatchOutcome, DispatchStatus,
    Dispatcher, DispatcherDrainReport, DispatcherStatsSnapshot, DlqEntry, InboxEnvelope,
};
pub(crate) use util::append_trigger_inbox_envelope;
pub use util::{
    append_dispatch_cancel_request, clear_dispatcher_state, enqueue_trigger_event,
    snapshot_dispatcher_stats,
};

/// Public wrapper for [`util::build_batched_event`]. Used by
/// `crate::channels` (CH-04 / #1875) to package aggregated events into a
/// single [`TriggerEvent`] before dispatch.
pub fn build_batched_event_public(
    events: Vec<TriggerEvent>,
) -> Result<TriggerEvent, DispatchError> {
    util::build_batched_event(events)
}

pub(crate) use state::{
    current_dispatch_context, current_dispatch_is_replay, current_dispatch_wait_lease, is_replay,
};
pub(crate) use types::{DispatchContext, DispatchWaitLease, TriggerInboxTopicScope};

use action_graph::{
    dispatch_entry_edge_kind, dispatch_error_label, dispatch_error_metadata, dispatch_node_id,
    dispatch_node_kind, dispatch_node_label, dispatch_node_metadata, dispatch_success_metadata,
    dispatch_success_outcome, dispatch_target_agent, retry_node_metadata,
    trigger_event_persona_metadata, trigger_node_metadata,
};
#[cfg(test)]
use state::install_test_inbox_dequeued_signal;
use state::{notify_test_inbox_dequeued, ACTIVE_DISPATCHER_STATE, ACTIVE_DISPATCH_IS_REPLAY};
use types::{
    AcquiredFlowControl, ConcurrencyLease, DispatchCallResult, DispatchSkipStage,
    DispatcherRuntimeState, FlowControlOutcome, SingletonLease, DEFAULT_AUTONOMY_BUDGET_REVIEWER,
};
use util::{
    accepted_at_ms, binding_key_from_parts, build_batched_event, cancelled_dispatch_outcome,
    current_unix_ms, decrement_in_flight, decrement_retry_queue_depth, dispatch_cancel_requested,
    dispatch_result_cost_usd_micros, dispatch_result_status, duration_between_ms, event_headers,
    extract_event_path, extracted_key_value, extracted_priority_value, json_value_to_gate,
    maybe_fail_before_outbox, now_rfc3339, now_unix_ms, queue_appended_at_ms, recv_cancel,
    sleep_or_cancel_or_request, tenant_id, unix_ms, worker_queue_priority,
};

pub const TRIGGER_ACCEPTED_AT_MS_HEADER: &str = "harn_trigger_accepted_at_ms";
pub const TRIGGER_NORMALIZED_AT_MS_HEADER: &str = "harn_trigger_normalized_at_ms";
pub const TRIGGER_QUEUE_APPENDED_AT_MS_HEADER: &str = "harn_trigger_queue_appended_at_ms";

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
            base_vm: Arc::new(base_vm),
            event_log,
            cancel_tx,
            state,
            metrics,
            a2a_client: Arc::new(crate::a2a::RealA2aClient),
        }
    }

    #[cfg(test)]
    pub fn with_a2a_client(mut self, client: Arc<dyn crate::a2a::A2aClient>) -> Self {
        self.a2a_client = client;
        self
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
        self.enqueue_targeted_with_headers(trigger_id, binding_version, event, None)
            .await
    }

    pub async fn enqueue_targeted_with_headers(
        &self,
        trigger_id: Option<String>,
        binding_version: Option<u32>,
        event: TriggerEvent,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<u64, DispatchError> {
        let trigger_id_for_metrics = trigger_id.clone();
        let mut headers = parent_headers.cloned().unwrap_or_default();
        headers.extend(event_headers(&event, None, None, None));
        if let Some(trigger_id) = trigger_id_for_metrics.as_ref() {
            headers.insert("trigger_id".to_string(), trigger_id.clone());
            headers.insert(
                "binding_key".to_string(),
                binding_key_from_parts(trigger_id, binding_version),
            );
        }
        headers
            .entry(TRIGGER_ACCEPTED_AT_MS_HEADER.to_string())
            .or_insert_with(|| unix_ms(event.received_at).to_string());
        let log_event = LogEvent::new("event_ingested", serde_json::Value::Null);
        let had_queue_appended_at = headers.contains_key(TRIGGER_QUEUE_APPENDED_AT_MS_HEADER);
        let queue_appended_at_ms = headers
            .get(TRIGGER_QUEUE_APPENDED_AT_MS_HEADER)
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(log_event.occurred_at_ms);
        headers
            .entry(TRIGGER_QUEUE_APPENDED_AT_MS_HEADER.to_string())
            .or_insert_with(|| log_event.occurred_at_ms.to_string());
        if let (Some(metrics), Some(trigger_id)) =
            (self.metrics.as_ref(), trigger_id_for_metrics.as_ref())
        {
            let binding_key = binding_key_from_parts(trigger_id, binding_version);
            let accepted_at_ms = accepted_at_ms(Some(&headers), &event);
            if !had_queue_appended_at {
                metrics.record_trigger_accepted_to_queue_append(
                    trigger_id,
                    &binding_key,
                    event.provider.as_str(),
                    tenant_id(&event),
                    "queued",
                    duration_between_ms(queue_appended_at_ms, accepted_at_ms),
                );
            }
            metrics.note_trigger_pending_event(
                event.id.0.as_str(),
                trigger_id,
                &binding_key,
                event.provider.as_str(),
                tenant_id(&event),
                accepted_at_ms,
                queue_appended_at_ms,
            );
        }
        append_trigger_inbox_envelope(
            self.event_log.as_ref(),
            trigger_id,
            binding_version,
            &event,
            headers,
            TriggerInboxTopicScope::Tenant,
        )
        .await
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

    pub async fn dispatch_inbox_envelope_with_parent_headers(
        &self,
        envelope: InboxEnvelope,
        parent_headers: &BTreeMap<String, String>,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        self.dispatch_inbox_envelope_with_headers(envelope, Some(parent_headers))
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

        self.dispatch_event_with_headers(envelope.event, parent_headers)
            .await
    }

    pub async fn dispatch_event(
        &self,
        event: TriggerEvent,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        self.dispatch_event_with_headers(event, None).await
    }

    async fn dispatch_event_with_headers(
        &self,
        event: TriggerEvent,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        let bindings = matching_bindings(&event);
        let mut outcomes = Vec::new();
        for binding in bindings {
            outcomes.push(
                self.dispatch_with_replay(&binding, event.clone(), None, None, parent_headers)
                    .await?,
            );
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
        let parent_headers_for_metrics = parent_headers.cloned();
        let admitted_at_ms = current_unix_ms();
        if let Some(metrics) = self.metrics.as_ref() {
            let binding_key = binding.binding_key();
            let queue_appended_at_ms = queue_appended_at_ms(parent_headers, &event);
            metrics.record_trigger_queue_age_at_dispatch_admission(
                binding.id.as_str(),
                &binding_key,
                event.provider.as_str(),
                tenant_id(&event),
                "admitted",
                duration_between_ms(admitted_at_ms, queue_appended_at_ms),
            );
            metrics.clear_trigger_pending_event(
                event.id.0.as_str(),
                binding.id.as_str(),
                &binding_key,
                event.provider.as_str(),
                tenant_id(&event),
                admitted_at_ms,
            );
        }
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
                self.dispatch_with_replay_inner(
                    binding,
                    event,
                    replay_of_event_id,
                    parent_headers_for_metrics,
                )
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
        parent_headers: Option<BTreeMap<String, String>>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let autonomy_tier = crate::resolve_agent_autonomy_tier(
            &self.event_log,
            binding.id.as_str(),
            binding.autonomy_tier,
        )
        .await
        .unwrap_or(binding.autonomy_tier);
        let autonomy_tier = binding.handler.effective_autonomy_tier(autonomy_tier);
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
                    kind: ACTION_GRAPH_NODE_KIND_PREDICATE.to_string(),
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
                if evaluation.exhaustion_strategy == Some(TriggerBudgetExhaustionStrategy::Fail) {
                    let final_error = format!(
                        "trigger budget exhausted: {}",
                        evaluation.reason.as_deref().unwrap_or("budget_exhausted")
                    );
                    self.move_budget_exhausted_to_dlq(
                        binding,
                        &route,
                        &event,
                        replay_of_event_id.as_ref(),
                        &predicate_node_id,
                        &final_error,
                    )
                    .await?;
                    finish_in_flight(
                        binding.id.as_str(),
                        binding.version,
                        TriggerDispatchOutcome::Dlq,
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
                        TrustOutcome::Failure,
                        "dlq",
                        0,
                        Some(final_error.clone()),
                    )
                    .await?;
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: 0,
                        status: DispatchStatus::Dlq,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
                        replay_of_event_id,
                        result: None,
                        error: Some(final_error),
                    });
                }

                if evaluation.exhaustion_strategy
                    == Some(TriggerBudgetExhaustionStrategy::RetryLater)
                {
                    self.append_budget_deferred_event(
                        binding,
                        &route,
                        &event,
                        replay_of_event_id.as_ref(),
                        DispatchSkipStage::Predicate,
                        evaluation.reason.as_deref().unwrap_or("budget_exhausted"),
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
                        "waiting",
                        0,
                        evaluation.reason.clone(),
                    )
                    .await?;
                    return Ok(DispatchOutcome {
                        trigger_id: binding.id.as_str().to_string(),
                        binding_key: binding.binding_key(),
                        event_id: event.id.0,
                        attempt_count: 0,
                        status: DispatchStatus::Waiting,
                        handler_kind: route.kind().to_string(),
                        target_uri: route.target_uri(),
                        replay_of_event_id,
                        result: Some(serde_json::json!({
                            "deferred": true,
                            "predicate": predicate.raw,
                            "reason": evaluation.reason,
                        })),
                        error: None,
                    });
                }

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

        if let Some(outcome) = self
            .handle_dispatch_budget_exhaustion(
                binding,
                &route,
                &event,
                replay_of_event_id.as_ref(),
                &source_node_id,
                autonomy_tier,
            )
            .await?
        {
            return Ok(outcome);
        }

        if autonomy_tier == AutonomyTier::ActAuto {
            if let Some(reason) = binding_autonomy_budget_would_exceed(binding) {
                let request_id = self
                    .append_autonomy_budget_approval_request(
                        binding,
                        &route,
                        &event,
                        replay_of_event_id.as_ref(),
                        reason,
                    )
                    .await?;
                self.emit_autonomy_budget_approval_action_graph(
                    binding,
                    &route,
                    &event,
                    &source_node_id,
                    replay_of_event_id.as_ref(),
                    reason,
                    &request_id,
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
                self.append_tier_transition_trust_record(
                    binding,
                    &event,
                    replay_of_event_id.as_ref(),
                    autonomy_tier,
                    AutonomyTier::ActWithApproval,
                    reason,
                    &request_id,
                )
                .await?;
                self.append_dispatch_trust_record(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id.as_ref(),
                    autonomy_tier,
                    TrustOutcome::Denied,
                    "waiting",
                    0,
                    Some(reason.to_string()),
                )
                .await?;
                return Ok(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0,
                    attempt_count: 0,
                    status: DispatchStatus::Waiting,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id,
                    result: Some(serde_json::json!({
                        "approval_required": true,
                        "request_id": request_id,
                        "reason": reason,
                        "reviewers": [DEFAULT_AUTONOMY_BUDGET_REVIEWER],
                    })),
                    error: None,
                });
            }
            note_autonomous_decision(binding);
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

        let destination_key = destination_circuit_key(&route);
        let half_open_probe = match self.state.destination_circuits.check(&destination_key) {
            DestinationCircuitProbe::Allow { half_open } => {
                if half_open {
                    if let Some(metrics) = self.metrics.as_ref() {
                        metrics.record_backpressure_event("circuit", "half_open_probe");
                    }
                }
                half_open
            }
            DestinationCircuitProbe::Block { retry_after } => {
                if let Some(metrics) = self.metrics.as_ref() {
                    metrics.record_backpressure_event("circuit", "fail_fast");
                }
                let final_error = format!(
                    "destination circuit open for {}; retry after {}s",
                    destination_key,
                    retry_after.as_secs().max(1)
                );
                self.move_circuit_open_to_dlq(
                    binding,
                    &route,
                    &event,
                    replay_of_event_id.as_ref(),
                    &final_error,
                    &destination_key,
                )
                .await?;
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Dlq,
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
                    "dlq",
                    0,
                    Some(final_error.clone()),
                )
                .await?;
                return Ok(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0,
                    attempt_count: 0,
                    status: DispatchStatus::Dlq,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id,
                    result: None,
                    error: Some(final_error),
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
            let attempt_started_instant = Instant::now();
            let attempt_started_at_ms = current_unix_ms();
            let queue_age_at_start = duration_between_ms(
                attempt_started_at_ms,
                queue_appended_at_ms(parent_headers.as_ref(), &event),
            );
            if attempt == 1 {
                if let Some(metrics) = self.metrics.as_ref() {
                    metrics.record_trigger_queue_age_at_dispatch_start(
                        binding.id.as_str(),
                        &binding_key,
                        event.provider.as_str(),
                        tenant_id(&event),
                        "started",
                        queue_age_at_start,
                    );
                }
            }
            tracing::info!(
                component = "dispatcher",
                lifecycle = "dispatch_started",
                trigger_id = %binding.id.as_str(),
                binding_key = %binding_key,
                event_id = %event.id.0,
                attempt,
                queue_age_ms = queue_age_at_start.as_millis(),
                trace_id = %event.trace_id.0
            );
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
            let attempt_runtime = attempt_started_instant.elapsed();
            let attempt_status = dispatch_result_status(&result);
            if let Some(metrics) = self.metrics.as_ref() {
                metrics.record_trigger_dispatch_runtime(
                    binding.id.as_str(),
                    &binding_key,
                    event.provider.as_str(),
                    tenant_id(&event),
                    attempt_status,
                    attempt_runtime,
                );
            }
            tracing::info!(
                component = "dispatcher",
                lifecycle = "handler_completed",
                trigger_id = %binding.id.as_str(),
                binding_key = %binding_key,
                event_id = %event.id.0,
                attempt,
                status = attempt_status,
                runtime_ms = attempt_runtime.as_millis(),
                trace_id = %event.trace_id.0
            );
            let completed_at = now_rfc3339();

            match result {
                Ok(dispatch_result) => {
                    let result = dispatch_result.output;
                    let mut dispatch_metadata = dispatch_result.metadata;
                    self.note_dispatch_result_cost(binding, &result, &mut dispatch_metadata);
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
                            "dispatch_metadata": dispatch_metadata,
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
                            "dispatch_metadata": dispatch_metadata,
                            "result": result,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                        replay_of_event_id.as_ref(),
                    )
                    .await?;
                    let mut node_metadata =
                        dispatch_success_metadata(&route, binding, &event, attempt, &result);
                    node_metadata.extend(dispatch_metadata.clone());
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
                            metadata: node_metadata,
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
                            "dispatch_metadata": dispatch_metadata,
                            "result": result,
                            "replay_of_event_id": replay_of_event_id,
                        }),
                    )
                    .await?;
                    self.state
                        .destination_circuits
                        .record_success(&destination_key);
                    if half_open_probe {
                        if let Some(metrics) = self.metrics.as_ref() {
                            metrics.record_backpressure_event("circuit", "closed");
                        }
                    }
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

                    let circuit_opened = if error.retryable() {
                        self.state
                            .destination_circuits
                            .record_failure(&destination_key)
                    } else {
                        false
                    };
                    if circuit_opened {
                        if let Some(metrics) = self.metrics.as_ref() {
                            metrics.record_backpressure_event("circuit", "opened");
                            metrics.record_trigger_dlq(binding.id.as_str(), "circuit_open");
                            metrics.record_trigger_accepted_to_dlq(
                                binding.id.as_str(),
                                &binding_key,
                                event.provider.as_str(),
                                tenant_id(&event),
                                "circuit_open",
                                duration_between_ms(
                                    current_unix_ms(),
                                    accepted_at_ms(parent_headers.as_ref(), &event),
                                ),
                            );
                        }
                        let final_error = format!(
                            "destination circuit opened for {destination_key} after {DESTINATION_CIRCUIT_FAILURE_THRESHOLD} consecutive failures: {error}"
                        );
                        let dlq_entry = DlqEntry {
                            trigger_id: binding.id.as_str().to_string(),
                            binding_key: binding.binding_key(),
                            event: event.clone(),
                            attempt_count: attempt,
                            final_error: final_error.clone(),
                            error_class: crate::triggers::classify_trigger_dlq_error(&final_error)
                                .to_string(),
                            attempts: attempts.clone(),
                        };
                        self.state
                            .dlq
                            .lock()
                            .expect("dispatcher dlq poisoned")
                            .push(dlq_entry.clone());
                        self.append_lifecycle_event(
                            "DlqMoved",
                            &event,
                            binding,
                            serde_json::json!({
                                "event_id": event.id.0,
                                "attempt_count": attempt,
                                "final_error": dlq_entry.final_error,
                                "reason": "circuit_open",
                                "destination": destination_key,
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
                            serde_json::to_value(&dlq_entry).map_err(|serde_error| {
                                DispatchError::Serde(serde_error.to_string())
                            })?,
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
                            Some(final_error.clone()),
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
                            error: Some(final_error),
                        });
                    }

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
                            metrics.record_trigger_retry_delay(
                                binding.id.as_str(),
                                &binding_key,
                                event.provider.as_str(),
                                tenant_id(&event),
                                "scheduled",
                                delay,
                            );
                        }
                        tracing::info!(
                            component = "dispatcher",
                            lifecycle = "retry_scheduled",
                            trigger_id = %binding.id.as_str(),
                            binding_key = %binding_key,
                            event_id = %event.id.0,
                            attempt = attempt + 1,
                            delay_ms = delay.as_millis(),
                            trace_id = %event.trace_id.0
                        );
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
                        error_class: crate::triggers::classify_trigger_dlq_error(&final_error)
                            .to_string(),
                        attempts: attempts.clone(),
                    };
                    self.state
                        .dlq
                        .lock()
                        .expect("dispatcher dlq poisoned")
                        .push(dlq_entry.clone());
                    if let Some(metrics) = self.metrics.as_ref() {
                        metrics.record_trigger_dlq(binding.id.as_str(), "retry_exhausted");
                        metrics.record_trigger_accepted_to_dlq(
                            binding.id.as_str(),
                            &binding_key,
                            event.provider.as_str(),
                            tenant_id(&event),
                            "retry_exhausted",
                            duration_between_ms(
                                current_unix_ms(),
                                accepted_at_ms(parent_headers.as_ref(), &event),
                            ),
                        );
                    }
                    tracing::info!(
                        component = "dispatcher",
                        lifecycle = "dlq_moved",
                        trigger_id = %binding.id.as_str(),
                        binding_key = %binding_key,
                        event_id = %event.id.0,
                        attempt_count = attempt,
                        reason = "retry_exhausted",
                        trace_id = %event.trace_id.0
                    );
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

    async fn handle_dispatch_budget_exhaustion(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        source_node_id: &str,
        autonomy_tier: AutonomyTier,
    ) -> Result<Option<DispatchOutcome>, DispatchError> {
        let expected_cost_usd_micros = 0;
        let Some(reason) = binding_budget_would_exceed(binding, expected_cost_usd_micros)
            .or_else(|| orchestrator_budget_would_exceed(expected_cost_usd_micros))
        else {
            return Ok(None);
        };
        self.append_trigger_budget_exhausted_event(
            binding,
            route,
            event,
            replay_of_event_id,
            reason,
            expected_cost_usd_micros,
        )
        .await?;

        if binding.on_budget_exhausted == TriggerBudgetExhaustionStrategy::Warn {
            return Ok(None);
        }

        match binding.on_budget_exhausted {
            TriggerBudgetExhaustionStrategy::Fail => {
                let final_error = format!("trigger budget exhausted: {reason}");
                self.move_budget_exhausted_to_dlq(
                    binding,
                    route,
                    event,
                    replay_of_event_id,
                    source_node_id,
                    &final_error,
                )
                .await?;
                finish_in_flight(
                    binding.id.as_str(),
                    binding.version,
                    TriggerDispatchOutcome::Dlq,
                )
                .await
                .map_err(|error| DispatchError::Registry(error.to_string()))?;
                decrement_in_flight(&self.state);
                self.append_dispatch_trust_record(
                    binding,
                    route,
                    event,
                    replay_of_event_id,
                    autonomy_tier,
                    TrustOutcome::Failure,
                    "dlq",
                    0,
                    Some(final_error.clone()),
                )
                .await?;
                Ok(Some(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0.clone(),
                    attempt_count: 0,
                    status: DispatchStatus::Dlq,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id: replay_of_event_id.cloned(),
                    result: None,
                    error: Some(final_error),
                }))
            }
            TriggerBudgetExhaustionStrategy::RetryLater => {
                self.append_budget_deferred_event(
                    binding,
                    route,
                    event,
                    replay_of_event_id,
                    DispatchSkipStage::Budget,
                    reason,
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
                    route,
                    event,
                    replay_of_event_id,
                    autonomy_tier,
                    TrustOutcome::Denied,
                    "waiting",
                    0,
                    Some(reason.to_string()),
                )
                .await?;
                Ok(Some(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0.clone(),
                    attempt_count: 0,
                    status: DispatchStatus::Waiting,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id: replay_of_event_id.cloned(),
                    result: Some(serde_json::json!({
                        "deferred": true,
                        "reason": reason,
                    })),
                    error: None,
                }))
            }
            TriggerBudgetExhaustionStrategy::False => {
                self.append_skipped_outbox_event(
                    binding,
                    route,
                    event,
                    replay_of_event_id,
                    DispatchSkipStage::Budget,
                    serde_json::json!({
                        "budget": reason,
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
                    route,
                    event,
                    replay_of_event_id,
                    autonomy_tier,
                    TrustOutcome::Denied,
                    "skipped",
                    0,
                    Some(reason.to_string()),
                )
                .await?;
                Ok(Some(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0.clone(),
                    attempt_count: 0,
                    status: DispatchStatus::Skipped,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id: replay_of_event_id.cloned(),
                    result: Some(serde_json::json!({
                        "skipped": true,
                        "budget": reason,
                    })),
                    error: None,
                }))
            }
            TriggerBudgetExhaustionStrategy::Warn => Ok(None),
        }
    }

    fn note_dispatch_result_cost(
        &self,
        binding: &TriggerBinding,
        result: &serde_json::Value,
        metadata: &mut BTreeMap<String, serde_json::Value>,
    ) {
        let cost_usd_micros = dispatch_result_cost_usd_micros(result);
        if cost_usd_micros == 0 {
            return;
        }
        note_binding_budget_cost(binding, cost_usd_micros);
        note_orchestrator_budget_cost(cost_usd_micros);
        metadata.insert(
            "cost_usd".to_string(),
            serde_json::json!(micros_to_usd(cost_usd_micros)),
        );
    }

    async fn dispatch_once(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        match route {
            DispatchUri::Local { .. } => {
                let TriggerHandlerSpec::Local { callable, .. } = &binding.handler else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to a local dispatch URI but does not carry a local closure",
                        binding.id.as_str()
                    )));
                };
                let value = self
                    .invoke_vm_callable(
                        callable,
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &event.qualified_kind(),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await?;
                Ok(DispatchCallResult {
                    output: vm_value_to_json(&value),
                    metadata: route.dispatch_boundary_metadata(),
                })
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
                let (endpoint, ack) = self
                    .a2a_client
                    .dispatch(
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
                        crate::a2a::A2aClientError::Denied(message) => {
                            DispatchError::Denied(message)
                        }
                        crate::a2a::A2aClientError::Timeout(message) => {
                            DispatchError::Timeout(message)
                        }
                        other => DispatchError::A2a(other.to_string()),
                    })?;
                let mut metadata = route.dispatch_boundary_metadata();
                metadata.insert(
                    "target_agent".to_string(),
                    serde_json::json!(endpoint.target_agent),
                );
                metadata.insert("card_url".to_string(), serde_json::json!(endpoint.card_url));
                metadata.insert("rpc_url".to_string(), serde_json::json!(endpoint.rpc_url));
                if let Some(agent_id) = endpoint.agent_id {
                    metadata.insert("remote_agent_id".to_string(), serde_json::json!(agent_id));
                }
                match ack {
                    crate::a2a::DispatchAck::InlineResult { task_id, result } => {
                        metadata.insert("task_id".to_string(), serde_json::json!(task_id));
                        metadata.insert("state".to_string(), serde_json::json!("completed"));
                        Ok(DispatchCallResult {
                            output: result,
                            metadata,
                        })
                    }
                    crate::a2a::DispatchAck::PendingTask {
                        task_id,
                        state,
                        handle,
                    } => {
                        metadata.insert("task_id".to_string(), serde_json::json!(task_id));
                        metadata.insert("state".to_string(), serde_json::json!(state));
                        Ok(DispatchCallResult {
                            output: handle,
                            metadata,
                        })
                    }
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
                let mut metadata = route.dispatch_boundary_metadata();
                metadata.insert("queue_name".to_string(), serde_json::json!(queue));
                Ok(DispatchCallResult {
                    output: serde_json::to_value(receipt)
                        .map_err(|error| DispatchError::Serde(error.to_string()))?,
                    metadata,
                })
            }
            DispatchUri::Persona { .. } => {
                self.dispatch_persona(binding, route, event, autonomy_tier, wait_lease, cancel_rx)
                    .await
            }
            DispatchUri::EvalPack { target, pack_id } => {
                let TriggerHandlerSpec::EvalPack {
                    manifest,
                    ledger_options,
                    ..
                } = &binding.handler
                else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to an eval_pack dispatch URI but does not carry an eval_pack manifest",
                        binding.id.as_str()
                    )));
                };
                let report = crate::orchestration::evaluate_eval_pack_manifest_resumable(
                    manifest,
                    ledger_options.clone(),
                )
                .map_err(|error| DispatchError::Local(error.to_string()))?;
                let mut metadata = route.dispatch_boundary_metadata();
                metadata.insert("eval_pack_target".to_string(), serde_json::json!(target));
                metadata.insert("eval_pack_id".to_string(), serde_json::json!(pack_id));
                Ok(DispatchCallResult {
                    output: serde_json::to_value(report)
                        .map_err(|error| DispatchError::Serde(error.to_string()))?,
                    metadata,
                })
            }
            DispatchUri::AutoResume { worker_id } => {
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.base_vm.child_vm());
                let value = crate::stdlib::agents::resume_worker_from_auto_resume_trigger(
                    &ctx, worker_id, event,
                )
                .await
                .map_err(|error| DispatchError::Local(error.to_string()))?;
                let mut metadata = route.dispatch_boundary_metadata();
                metadata.insert("worker_id".to_string(), serde_json::json!(worker_id));
                metadata.insert("resume_kind".to_string(), serde_json::json!("auto_resume"));
                Ok(DispatchCallResult {
                    output: vm_value_to_json(&value),
                    metadata,
                })
            }
            DispatchUri::SpawnToPool {
                pool,
                priority_from,
                key_from,
            } => {
                let TriggerHandlerSpec::SpawnToPool { task_factory, .. } = &binding.handler else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to a pool dispatch URI but does not carry a spawn_to_pool handler",
                        binding.id.as_str()
                    )));
                };
                self.dispatch_spawn_to_pool(
                    binding,
                    route,
                    event,
                    pool,
                    priority_from.as_deref(),
                    key_from.as_deref(),
                    task_factory,
                    autonomy_tier,
                    wait_lease,
                    cancel_rx,
                )
                .await
            }
            DispatchUri::ReminderInject { .. } => {
                let TriggerHandlerSpec::ReminderInject {
                    target,
                    body,
                    tags,
                    ttl_turns,
                    dedupe_key,
                    propagate,
                    role_hint,
                    preserve_on_compact,
                } = &binding.handler
                else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to a reminder_inject dispatch URI but does not carry a reminder_inject handler",
                        binding.id.as_str()
                    )));
                };
                self.dispatch_reminder_inject(
                    binding,
                    route,
                    event,
                    target,
                    body,
                    tags,
                    *ttl_turns,
                    dedupe_key.as_deref(),
                    *propagate,
                    *role_hint,
                    *preserve_on_compact,
                    autonomy_tier,
                    wait_lease,
                    cancel_rx,
                )
                .await
            }
            DispatchUri::InterruptAndSuspend { .. } => {
                let TriggerHandlerSpec::InterruptAndSuspend {
                    target_agents,
                    reason,
                } = &binding.handler
                else {
                    return Err(DispatchError::Local(format!(
                        "trigger '{}' resolved to an interrupt_and_suspend dispatch URI but does not carry an interrupt_and_suspend handler",
                        binding.id.as_str()
                    )));
                };
                self.dispatch_interrupt_and_suspend(
                    binding,
                    route,
                    event,
                    target_agents,
                    reason,
                    autonomy_tier,
                    wait_lease,
                    cancel_rx,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_spawn_to_pool(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        pool: &str,
        priority_from: Option<&str>,
        key_from: Option<&str>,
        task_factory: &Arc<crate::value::VmClosure>,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        // Step 1: invoke task_factory(event) via the standard VM invocation
        // path so policy intersection, cancellation, and dispatch context
        // bookkeeping all work the same way as a Local handler.
        let factory_value = self
            .invoke_vm_callable(
                &crate::value::VmCallable::Eager(Arc::clone(task_factory)),
                &binding.binding_key(),
                event,
                None,
                binding.id.as_str(),
                &event.qualified_kind(),
                autonomy_tier,
                wait_lease,
                cancel_rx,
            )
            .await?;

        // Step 2: the factory must return a closure. The dispatcher submits
        // that closure to the named pool; the pool runs it under its own
        // queue strategy + backpressure policy.
        let closure = match factory_value {
            crate::value::VmValue::Closure(closure) => closure,
            other => {
                return Err(DispatchError::Local(format!(
                    "trigger '{}': SpawnToPool task_factory must return a closure, got {}",
                    binding.id.as_str(),
                    other.type_name()
                )));
            }
        };

        // Step 3: derive priority + fair-queue key from the event payload
        // using simple dotted-path extractors. Missing/non-coercible paths
        // fall back to defaults (priority 0, null key) instead of erroring;
        // that matches how the upstream pool builtin treats omitted options
        // and keeps a misconfigured path from blocking the whole pipeline.
        let event_json =
            serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
        let priority = priority_from
            .and_then(|path| extract_event_path(&event_json, path))
            .and_then(extracted_priority_value)
            .unwrap_or(0);
        let key = key_from
            .and_then(|path| extract_event_path(&event_json, path))
            .and_then(extracted_key_value);

        // Step 4: submit to the pool. Pool errors (pool not found,
        // fail_fast/fail_submitter rejection) bubble up as DispatchError so
        // the surrounding retry/DLQ machinery applies the configured policy.
        let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.base_vm.child_vm());
        let outcome = crate::stdlib::pool::submit_closure_to_named_pool(
            Some(&ctx),
            pool,
            closure,
            priority,
            key.clone(),
        )
        .await
        .map_err(|error| DispatchError::Local(error.to_string()))?;

        let mut metadata = route.dispatch_boundary_metadata();
        metadata.insert("pool".to_string(), serde_json::json!(pool));
        metadata.insert("priority".to_string(), serde_json::json!(priority));
        if let Some(key) = &key {
            metadata.insert("key".to_string(), serde_json::json!(key));
        }

        let crate::stdlib::pool::PoolSubmitOutcome {
            pool_id,
            pool_name,
            task_id,
            status,
            rejection_reason,
        } = outcome;
        metadata.insert("pool_id".to_string(), serde_json::json!(pool_id));
        metadata.insert("pool_name".to_string(), serde_json::json!(pool_name));
        metadata.insert("task_id".to_string(), serde_json::json!(task_id));
        metadata.insert("status".to_string(), serde_json::json!(status));

        // Output dict doubles as a pool_task handle (`_type: "pool_task"`)
        // so Harn handlers can `pool_wait(dispatch.result)` directly — the
        // same shape `__pool_submit` returns from a pool.submit() call.
        let mut output = serde_json::Map::new();
        output.insert("_type".to_string(), serde_json::json!("pool_task"));
        output.insert("id".to_string(), serde_json::json!(task_id));
        output.insert("pool_id".to_string(), serde_json::json!(pool_id));
        output.insert("pool".to_string(), serde_json::json!(pool_name));
        output.insert("status".to_string(), serde_json::json!(status));
        output.insert("priority".to_string(), serde_json::json!(priority));
        if let Some(key) = &key {
            output.insert("key".to_string(), serde_json::json!(key));
        }
        if let Some(reason) = rejection_reason {
            output.insert("rejection_reason".to_string(), serde_json::json!(reason));
            metadata.insert("rejection_reason".to_string(), serde_json::json!(reason));
        }
        Ok(DispatchCallResult {
            output: serde_json::Value::Object(output),
            metadata,
        })
    }

    /// ReminderInject (#1876) — resolve the target running session, render
    /// the body template against the event payload, build a `SystemReminder`
    /// from the binding's reminder metadata, and inject it via the existing
    /// `agent_sessions::inject_reminder` pipeline. Missing-target dispatches
    /// emit a `triggers.reminder_inject.audit` audit entry tagged
    /// `target_missing` and return a `dropped` outcome instead of failing
    /// the dispatch — that matches the "drop the reminder + emit audit"
    /// failure-mode contract in the #1876 spec.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_reminder_inject(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        target: &TargetExpr,
        body_template: &str,
        tags: &[String],
        ttl_turns: Option<i64>,
        dedupe_key: Option<&str>,
        propagate: crate::llm::helpers::ReminderPropagate,
        role_hint: crate::llm::helpers::ReminderRoleHint,
        preserve_on_compact: bool,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        // Step 1: resolve the target session. Closure resolution runs through
        // the standard VM invocation path so dispatch context, cancellation,
        // and policy intersection apply uniformly with other handler variants.
        let resolved_target = match target {
            TargetExpr::Current => crate::agent_sessions::current_session_id(),
            TargetExpr::Parent => crate::agent_sessions::current_session_id()
                .and_then(|id| crate::agent_sessions::parent_id(&id)),
            TargetExpr::Concrete(id) => Some(id.clone()),
            TargetExpr::Closure(closure) => {
                let value = self
                    .invoke_vm_callable(
                        &crate::value::VmCallable::Eager(Arc::clone(closure)),
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &event.qualified_kind(),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await?;
                match value {
                    crate::value::VmValue::String(s) => {
                        let id = s.trim().to_string();
                        if id.is_empty() {
                            None
                        } else {
                            Some(id)
                        }
                    }
                    crate::value::VmValue::Nil => None,
                    other => {
                        return Err(DispatchError::Local(format!(
                            "trigger '{}': ReminderInject target closure must return a string session id or nil, got {}",
                            binding.id.as_str(),
                            other.type_name()
                        )));
                    }
                }
            }
        };

        // Step 2: render the body template. Bindings expose `event` (with the
        // full serialized event), `match` (matched_at), and a top-level
        // `batch` projection when flow-control batching turned this dispatch
        // into a synthetic batched event. Missing identifiers preserve the
        // pre-v2 silent-passthrough contract in the template engine, so
        // body strings without `{{ ... }}` round-trip unchanged.
        let event_json =
            serde_json::to_value(event).map_err(|error| DispatchError::Serde(error.to_string()))?;
        let mut bindings = crate::value::DictMap::new();
        bindings.insert(
            crate::value::intern_key("event"),
            crate::schema::json_to_vm_value(&event_json),
        );
        bindings.insert(
            crate::value::intern_key("match"),
            crate::schema::json_to_vm_value(&serde_json::json!({
                "matched_at": event_json.get("received_at").cloned().unwrap_or(serde_json::Value::Null),
            })),
        );
        if let Some(batch_value) = event_json.get("batch").or_else(|| {
            event_json
                .get("provider_payload")
                .and_then(|payload| payload.get("batch"))
        }) {
            bindings.insert(
                crate::value::intern_key("batch"),
                crate::schema::json_to_vm_value(batch_value),
            );
        }
        let rendered_body = crate::stdlib::template::render_template_to_string(
            body_template,
            Some(&bindings),
            None,
            None,
        )
        .map_err(|error| {
            DispatchError::Local(format!(
                "trigger '{}': ReminderInject template render failed: {error}",
                binding.id.as_str()
            ))
        })?;

        // Step 3: handle missing target as a graceful drop with audit + a
        // `dropped` dispatch result. The action-graph success_outcome maps
        // `status: "dropped"` to a distinct `dropped` label so observers
        // can tell a no-op from a successful inject without re-reading the
        // result blob.
        let target_kind = target.kind();
        let Some(target_session_id) = resolved_target else {
            crate::orchestration::record_lifecycle_audit(
                "triggers.reminder_inject.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": "dropped",
                    "reason": "target_missing",
                    "target_kind": target_kind,
                }),
            );
            let mut metadata = route.dispatch_boundary_metadata();
            metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
            metadata.insert("status".to_string(), serde_json::json!("dropped"));
            metadata.insert(
                "rejection_reason".to_string(),
                serde_json::json!("target_missing"),
            );
            let mut output = serde_json::Map::new();
            output.insert("status".to_string(), serde_json::json!("dropped"));
            output.insert("reason".to_string(), serde_json::json!("target_missing"));
            output.insert("target_kind".to_string(), serde_json::json!(target_kind));
            return Ok(DispatchCallResult {
                output: serde_json::Value::Object(output),
                metadata,
            });
        };

        // Step 4: target must exist as a registered session. Same graceful
        // drop semantics as a fully-missing target.
        if !crate::agent_sessions::exists(&target_session_id) {
            crate::orchestration::record_lifecycle_audit(
                "triggers.reminder_inject.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": "dropped",
                    "reason": "target_unknown_session",
                    "target_kind": target_kind,
                    "target_session_id": &target_session_id,
                }),
            );
            let mut metadata = route.dispatch_boundary_metadata();
            metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
            metadata.insert(
                "target_session_id".to_string(),
                serde_json::json!(&target_session_id),
            );
            metadata.insert("status".to_string(), serde_json::json!("dropped"));
            metadata.insert(
                "rejection_reason".to_string(),
                serde_json::json!("target_unknown_session"),
            );
            let mut output = serde_json::Map::new();
            output.insert("status".to_string(), serde_json::json!("dropped"));
            output.insert(
                "reason".to_string(),
                serde_json::json!("target_unknown_session"),
            );
            output.insert("target_kind".to_string(), serde_json::json!(target_kind));
            output.insert(
                "target_session_id".to_string(),
                serde_json::json!(&target_session_id),
            );
            return Ok(DispatchCallResult {
                output: serde_json::Value::Object(output),
                metadata,
            });
        }

        // Step 5: build the SystemReminder + inject via the canonical
        // agent_sessions pipeline. Reminders are sourced as `in_pipeline`
        // so the rendering machinery treats them the same as any other
        // dispatch-side reminder.
        let reminder = crate::llm::helpers::SystemReminder {
            id: uuid::Uuid::now_v7().to_string(),
            tags: tags.to_vec(),
            dedupe_key: dedupe_key.map(str::to_string),
            ttl_turns,
            preserve_on_compact,
            propagate,
            role_hint,
            source: crate::llm::helpers::ReminderSource::InPipeline,
            body: rendered_body,
            fired_at_turn: 0,
            originating_agent_id: None,
        };
        let reminder_id = reminder.id.clone();
        let report = crate::agent_sessions::inject_reminder(&target_session_id, reminder)
            .map_err(DispatchError::Local)?;

        crate::orchestration::record_lifecycle_audit(
            "triggers.reminder_inject.audit",
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "outcome": "injected",
                "target_kind": target_kind,
                "target_session_id": &target_session_id,
                "reminder_id": &reminder_id,
                "deduped_count": report.deduped_count,
            }),
        );

        let mut metadata = route.dispatch_boundary_metadata();
        metadata.insert("target_kind".to_string(), serde_json::json!(target_kind));
        metadata.insert(
            "target_session_id".to_string(),
            serde_json::json!(&target_session_id),
        );
        metadata.insert("status".to_string(), serde_json::json!("injected"));
        metadata.insert("reminder_id".to_string(), serde_json::json!(&reminder_id));
        if report.deduped_count > 0 {
            metadata.insert(
                "deduped_count".to_string(),
                serde_json::json!(report.deduped_count),
            );
        }
        let mut output = serde_json::Map::new();
        output.insert("status".to_string(), serde_json::json!("injected"));
        output.insert(
            "target_session_id".to_string(),
            serde_json::json!(&target_session_id),
        );
        output.insert("target_kind".to_string(), serde_json::json!(target_kind));
        output.insert("reminder_id".to_string(), serde_json::json!(&reminder_id));
        output.insert(
            "deduped_count".to_string(),
            serde_json::json!(report.deduped_count),
        );
        Ok(DispatchCallResult {
            output: serde_json::Value::Object(output),
            metadata,
        })
    }

    /// CH-10 (#1910) — emergency panic broadcast. Resolves
    /// `target_agents` into a concrete worker-id list (closure form is run
    /// through the standard VM invocation path so policy intersection and
    /// dispatch context match every other handler variant) and then runs
    /// the cooperative-suspend path on each target. Per-target outcomes
    /// (`suspended`/`already_suspended`/`not_running`/`unknown`) roll up
    /// into per-worker `triggers.interrupt_and_suspend.audit` audits plus a
    /// single summary audit. Empty target lists are a graceful no-op:
    /// `status: "broadcast"`, `suspended_count: 0`, and a `target_count: 0`
    /// audit so observers see the trigger fired but the registry was
    /// empty. The `reason` from the handler spec is propagated to every
    /// suspension envelope and audit entry.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_interrupt_and_suspend(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        target_agents: &AgentScope,
        reason: &str,
        autonomy_tier: AutonomyTier,
        wait_lease: Option<DispatchWaitLease>,
        cancel_rx: &mut broadcast::Receiver<()>,
    ) -> Result<DispatchCallResult, DispatchError> {
        // Step 1: resolve the target worker-id list. Closure resolution
        // runs through the standard VM invocation path so dispatch
        // context, cancellation, and policy intersection apply uniformly
        // with other handler variants. The closure must return a list of
        // strings; nil and empty-list are valid and turn into the
        // "no targets" graceful no-op below.
        let scope_kind = target_agents.kind();
        let target_ids: Vec<String> = match target_agents {
            AgentScope::All => crate::stdlib::agents::all_registered_worker_ids(),
            AgentScope::Concrete(ids) => ids.clone(),
            AgentScope::Closure(closure) => {
                let value = self
                    .invoke_vm_callable(
                        &crate::value::VmCallable::Eager(Arc::clone(closure)),
                        &binding.binding_key(),
                        event,
                        None,
                        binding.id.as_str(),
                        &event.qualified_kind(),
                        autonomy_tier,
                        wait_lease,
                        cancel_rx,
                    )
                    .await?;
                match value {
                    crate::value::VmValue::Nil => Vec::new(),
                    crate::value::VmValue::List(items) => {
                        let mut ids = Vec::with_capacity(items.len());
                        for item in items.iter() {
                            match item {
                                crate::value::VmValue::String(s) => {
                                    let id = s.trim().to_string();
                                    if !id.is_empty() {
                                        ids.push(id);
                                    }
                                }
                                other => {
                                    return Err(DispatchError::Local(format!(
                                        "trigger '{}': InterruptAndSuspend target closure list entries must be strings, got {}",
                                        binding.id.as_str(),
                                        other.type_name()
                                    )));
                                }
                            }
                        }
                        ids
                    }
                    other => {
                        return Err(DispatchError::Local(format!(
                            "trigger '{}': InterruptAndSuspend target closure must return a list of worker-id strings or nil, got {}",
                            binding.id.as_str(),
                            other.type_name()
                        )));
                    }
                }
            }
        };

        // Step 2: empty target list — graceful no-op. Single roll-up
        // audit so observers see the trigger fired against an empty
        // scope (e.g. a panic broadcast that arrived after every worker
        // already completed or was closed) rather than silently
        // succeeding.
        if target_ids.is_empty() {
            crate::orchestration::record_lifecycle_audit(
                "triggers.interrupt_and_suspend.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": "broadcast",
                    "scope_kind": scope_kind,
                    "reason": reason,
                    "target_count": 0,
                    "suspended_count": 0,
                    "skipped_count": 0,
                }),
            );
            let mut metadata = route.dispatch_boundary_metadata();
            metadata.insert("scope_kind".to_string(), serde_json::json!(scope_kind));
            metadata.insert("status".to_string(), serde_json::json!("broadcast"));
            metadata.insert("target_count".to_string(), serde_json::json!(0));
            metadata.insert("suspended_count".to_string(), serde_json::json!(0));
            metadata.insert("skipped_count".to_string(), serde_json::json!(0));
            metadata.insert("reason".to_string(), serde_json::json!(reason));
            let mut output = serde_json::Map::new();
            output.insert("status".to_string(), serde_json::json!("broadcast"));
            output.insert("scope_kind".to_string(), serde_json::json!(scope_kind));
            output.insert("target_count".to_string(), serde_json::json!(0));
            output.insert("suspended_count".to_string(), serde_json::json!(0));
            output.insert("skipped_count".to_string(), serde_json::json!(0));
            output.insert("reason".to_string(), serde_json::json!(reason));
            return Ok(DispatchCallResult {
                output: serde_json::Value::Object(output),
                metadata,
            });
        }

        // Step 3: per-target suspend. `panic_suspend_worker` is the
        // bypass-the-turn-boundary cooperative-suspend path that mirrors
        // `suspend_agent` minus the PreSuspend/PostSuspend hook gate
        // (panic is the explicit org-wide override). Per-target outcomes
        // are rolled into the per-worker audit and the dispatch summary.
        let mut suspended = 0u32;
        let mut skipped = 0u32;
        let mut per_worker = Vec::with_capacity(target_ids.len());
        let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.base_vm.child_vm());
        for worker_id in &target_ids {
            let outcome =
                crate::stdlib::agents::panic_suspend_worker(Some(&ctx), worker_id, reason)
                    .await
                    .map_err(|error| DispatchError::Local(error.to_string()))?;
            match outcome {
                crate::stdlib::agents::PanicSuspendOutcome::Suspended => {
                    suspended += 1;
                }
                _ => {
                    skipped += 1;
                }
            }
            crate::orchestration::record_lifecycle_audit(
                "triggers.interrupt_and_suspend.audit",
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "binding_key": binding.binding_key(),
                    "event_id": event.id.0,
                    "outcome": outcome.as_str(),
                    "scope_kind": scope_kind,
                    "reason": reason,
                    "worker_id": worker_id,
                }),
            );
            per_worker.push(serde_json::json!({
                "worker_id": worker_id,
                "outcome": outcome.as_str(),
            }));
        }

        // Step 4: single summary audit so observers can see the
        // broadcast roll-up without re-aggregating per-worker entries.
        crate::orchestration::record_lifecycle_audit(
            "triggers.interrupt_and_suspend.audit",
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "outcome": "broadcast",
                "scope_kind": scope_kind,
                "reason": reason,
                "target_count": target_ids.len(),
                "suspended_count": suspended,
                "skipped_count": skipped,
            }),
        );

        let mut metadata = route.dispatch_boundary_metadata();
        metadata.insert("scope_kind".to_string(), serde_json::json!(scope_kind));
        metadata.insert("status".to_string(), serde_json::json!("broadcast"));
        metadata.insert(
            "target_count".to_string(),
            serde_json::json!(target_ids.len()),
        );
        metadata.insert("suspended_count".to_string(), serde_json::json!(suspended));
        metadata.insert("skipped_count".to_string(), serde_json::json!(skipped));
        metadata.insert("reason".to_string(), serde_json::json!(reason));

        let mut output = serde_json::Map::new();
        output.insert("status".to_string(), serde_json::json!("broadcast"));
        output.insert("scope_kind".to_string(), serde_json::json!(scope_kind));
        output.insert(
            "target_count".to_string(),
            serde_json::json!(target_ids.len()),
        );
        output.insert("suspended_count".to_string(), serde_json::json!(suspended));
        output.insert("skipped_count".to_string(), serde_json::json!(skipped));
        output.insert("reason".to_string(), serde_json::json!(reason));
        output.insert("targets".to_string(), serde_json::Value::Array(per_worker));
        Ok(DispatchCallResult {
            output: serde_json::Value::Object(output),
            metadata,
        })
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
                &expr.callable,
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
}

#[cfg(test)]
mod tests;
