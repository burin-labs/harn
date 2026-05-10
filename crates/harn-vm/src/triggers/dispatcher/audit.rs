use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use crate::event_log::{EventLog, LogEvent};
use crate::orchestration::{RunActionGraphEdgeRecord, RunActionGraphNodeRecord};
use crate::triggers::registry::TriggerBinding;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};

use super::types::{
    DispatchAttemptRecord, DispatchError, DispatchSkipStage, Dispatcher,
    DEFAULT_AUTONOMY_BUDGET_REVIEWER,
};
use super::uri::DispatchUri;
use super::util::{
    dispatch_error_from_vm_error, event_headers, next_budget_reset_rfc3339, topic_for_event,
};
use super::TriggerEvent;
use super::{TRIGGERS_LIFECYCLE_TOPIC, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_OUTBOX_TOPIC};

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn append_dispatch_trust_record(
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

    pub(super) async fn append_attempt_record(
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

    pub(super) async fn append_lifecycle_event(
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

    pub(super) async fn append_autonomy_budget_approval_request(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        reason: &str,
    ) -> Result<String, DispatchError> {
        let reviewers = vec![DEFAULT_AUTONOMY_BUDGET_REVIEWER.to_string()];
        let detail = serde_json::json!({
            "trigger_id": binding.id.as_str(),
            "binding_key": binding.binding_key(),
            "event_id": event.id.0,
            "reason": reason,
            "from_tier": AutonomyTier::ActAuto.as_str(),
            "requested_tier": AutonomyTier::ActWithApproval.as_str(),
            "handler_kind": route.kind(),
            "target_uri": route.target_uri(),
            "max_autonomous_decisions_per_hour": binding.max_autonomous_decisions_per_hour,
            "max_autonomous_decisions_per_day": binding.max_autonomous_decisions_per_day,
            "autonomous_decisions_hour": binding.metrics.autonomous_decisions_hour.load(Ordering::Relaxed),
            "autonomous_decisions_today": binding.metrics.autonomous_decisions_today.load(Ordering::Relaxed),
            "replay_of_event_id": replay_of_event_id,
        });
        let request_id = crate::stdlib::hitl::append_approval_request_on(
            &self.event_log,
            binding.id.as_str().to_string(),
            event.trace_id.0.clone(),
            format!(
                "approve autonomous dispatch for trigger '{}' after {}",
                binding.id.as_str(),
                reason
            ),
            detail.clone(),
            reviewers.clone(),
        )
        .await
        .map_err(dispatch_error_from_vm_error)?;
        self.append_lifecycle_event(
            "autonomy.budget_exceeded",
            event,
            binding,
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "event_id": event.id.0,
                "reason": reason,
                "request_id": request_id,
                "reviewers": reviewers,
                "from_tier": AutonomyTier::ActAuto.as_str(),
                "requested_tier": AutonomyTier::ActWithApproval.as_str(),
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await?;
        Ok(request_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn emit_autonomy_budget_approval_action_graph(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        source_node_id: &str,
        replay_of_event_id: Option<&String>,
        reason: &str,
        request_id: &str,
    ) -> Result<(), DispatchError> {
        let approval_node_id = format!("approval:{}:{}", binding.binding_key(), event.id.0);
        self.emit_action_graph(
            event,
            vec![RunActionGraphNodeRecord {
                id: approval_node_id.clone(),
                label: format!("approval required: {reason}"),
                kind: "approval".to_string(),
                status: "waiting".to_string(),
                outcome: "request_approval".to_string(),
                trace_id: Some(event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: None,
                run_path: None,
                metadata: BTreeMap::from([
                    (
                        "trigger_id".to_string(),
                        serde_json::json!(binding.id.as_str()),
                    ),
                    (
                        "binding_key".to_string(),
                        serde_json::json!(binding.binding_key()),
                    ),
                    ("event_id".to_string(), serde_json::json!(event.id.0)),
                    ("reason".to_string(), serde_json::json!(reason)),
                    ("request_id".to_string(), serde_json::json!(request_id)),
                    (
                        "reviewers".to_string(),
                        serde_json::json!([DEFAULT_AUTONOMY_BUDGET_REVIEWER]),
                    ),
                    ("handler_kind".to_string(), serde_json::json!(route.kind())),
                    (
                        "target_uri".to_string(),
                        serde_json::json!(route.target_uri()),
                    ),
                    (
                        "from_tier".to_string(),
                        serde_json::json!(AutonomyTier::ActAuto.as_str()),
                    ),
                    (
                        "requested_tier".to_string(),
                        serde_json::json!(AutonomyTier::ActWithApproval.as_str()),
                    ),
                    (
                        "replay_of_event_id".to_string(),
                        serde_json::json!(replay_of_event_id),
                    ),
                ]),
            }],
            vec![RunActionGraphEdgeRecord {
                from_id: source_node_id.to_string(),
                to_id: approval_node_id,
                kind: "approval_gate".to_string(),
                label: Some("autonomy budget".to_string()),
            }],
            serde_json::json!({
                "source": "dispatcher",
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "event_id": event.id.0,
                "reason": reason,
                "request_id": request_id,
                "replay_of_event_id": replay_of_event_id,
            }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn append_tier_transition_trust_record(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        from_tier: AutonomyTier,
        to_tier: AutonomyTier,
        reason: &str,
        request_id: &str,
    ) -> Result<(), DispatchError> {
        let mut record = TrustRecord::new(
            binding.id.as_str().to_string(),
            "autonomy.tier_transition",
            Some(DEFAULT_AUTONOMY_BUDGET_REVIEWER.to_string()),
            TrustOutcome::Denied,
            event.trace_id.0.clone(),
            to_tier,
        );
        record.metadata.insert(
            "binding_key".to_string(),
            serde_json::json!(binding.binding_key()),
        );
        record
            .metadata
            .insert("event_id".to_string(), serde_json::json!(event.id.0));
        record.metadata.insert(
            "from_tier".to_string(),
            serde_json::json!(from_tier.as_str()),
        );
        record
            .metadata
            .insert("to_tier".to_string(), serde_json::json!(to_tier.as_str()));
        record
            .metadata
            .insert("reason".to_string(), serde_json::json!(reason));
        record
            .metadata
            .insert("request_id".to_string(), serde_json::json!(request_id));
        if let Some(replay_of_event_id) = replay_of_event_id {
            record.metadata.insert(
                "replay_of_event_id".to_string(),
                serde_json::json!(replay_of_event_id),
            );
        }
        append_trust_record(&self.event_log, &record)
            .await
            .map(|_| ())
            .map_err(DispatchError::from)
    }

    pub(super) async fn append_budget_deferred_event(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        reason: &str,
    ) -> Result<(), DispatchError> {
        self.append_topic_event(
            TRIGGER_ATTEMPTS_TOPIC,
            "budget_deferred",
            event,
            Some(binding),
            None,
            serde_json::json!({
                "event_id": event.id.0,
                "trigger_id": binding.id.as_str(),
                "binding_key": binding.binding_key(),
                "handler_kind": route.kind(),
                "target_uri": route.target_uri(),
                "reason": reason,
                "retry_at": next_budget_reset_rfc3339(binding),
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await?;
        self.append_skipped_outbox_event(
            binding,
            route,
            event,
            replay_of_event_id,
            DispatchSkipStage::Predicate,
            serde_json::json!({
                "deferred": true,
                "reason": reason,
                "retry_at": next_budget_reset_rfc3339(binding),
            }),
        )
        .await
    }

    pub(super) async fn append_skipped_outbox_event(
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

    pub(super) async fn append_topic_event(
        &self,
        topic_name: &str,
        kind: &str,
        event: &TriggerEvent,
        binding: Option<&TriggerBinding>,
        attempt: Option<u32>,
        payload: serde_json::Value,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        let topic = topic_for_event(event, topic_name)?;
        let headers = event_headers(event, binding, attempt, replay_of_event_id);
        self.event_log
            .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
            .await
            .map_err(DispatchError::from)
            .map(|_| ())
    }
}
