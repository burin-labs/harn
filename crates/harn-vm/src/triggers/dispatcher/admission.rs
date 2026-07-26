//! Pre-attempt admission gates: may this binding act on this event at all?
//!
//! Each gate runs before the dispatcher takes any flow-control lease or touches
//! the destination: an outstanding cancel request, the binding\'s `when`
//! predicate, the binding/orchestrator spend budgets, and the autonomy-tier
//! approval ceiling. A gate that stops the dispatch returns the terminal
//! [`DispatchOutcome`] the caller should hand back; passing every gate returns
//! `None`.
//!
//! Resource admission — flow control and destination circuits — deliberately
//! lives with the attempt loop instead, because the leases it takes have to be
//! released by the same code that runs and retries the attempts.

use super::*;

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_admission_gates(
        &self,
        binding: &TriggerBinding,
        route: &DispatchUri,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        autonomy_tier: AutonomyTier,
        source_node_id: &mut String,
    ) -> Result<Option<DispatchOutcome>, DispatchError> {
        let binding_key = binding.binding_key();
        if dispatch_cancel_requested(
            &self.event_log,
            &binding_key,
            &event.id.0,
            replay_of_event_id,
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
            return Ok(Some(cancelled_dispatch_outcome(
                binding,
                route,
                event,
                replay_of_event_id.cloned(),
                0,
                "trigger cancel request cancelled dispatch before attempt 1".to_string(),
            )));
        }

        if let Some(predicate) = binding.when.as_ref() {
            let predicate_node_id = format!("predicate:{binding_key}:{}", event.id.0);
            let evaluation = self
                .evaluate_predicate(binding, predicate, event, replay_of_event_id, autonomy_tier)
                .await?;
            let passed = evaluation.result;
            self.emit_action_graph(
                event,
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
                    metadata: predicate_node_metadata(binding, predicate, event, &evaluation),
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
                        route,
                        event,
                        replay_of_event_id,
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
                    return Ok(Some(DispatchOutcome {
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
                    }));
                }

                if evaluation.exhaustion_strategy
                    == Some(TriggerBudgetExhaustionStrategy::RetryLater)
                {
                    self.append_budget_deferred_event(
                        binding,
                        route,
                        event,
                        replay_of_event_id,
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
                        route,
                        event,
                        replay_of_event_id,
                        autonomy_tier,
                        TrustOutcome::Denied,
                        "waiting",
                        0,
                        evaluation.reason.clone(),
                    )
                    .await?;
                    return Ok(Some(DispatchOutcome {
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
                            "predicate": predicate.raw,
                            "reason": evaluation.reason,
                        })),
                        error: None,
                    }));
                }

                self.append_skipped_outbox_event(
                    binding,
                    route,
                    event,
                    replay_of_event_id,
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
                    route,
                    event,
                    replay_of_event_id,
                    autonomy_tier,
                    TrustOutcome::Denied,
                    "skipped",
                    0,
                    None,
                )
                .await?;
                return Ok(Some(DispatchOutcome {
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
                        "predicate": predicate.raw,
                        "reason": evaluation.reason,
                    })),
                    error: None,
                }));
            }

            *source_node_id = predicate_node_id;
        }

        if let Some(outcome) = self
            .handle_dispatch_budget_exhaustion(
                binding,
                route,
                event,
                replay_of_event_id,
                source_node_id.as_str(),
                autonomy_tier,
            )
            .await?
        {
            return Ok(Some(outcome));
        }

        if autonomy_tier == AutonomyTier::ActAuto {
            if let Some(reason) = binding_autonomy_budget_would_exceed(binding) {
                let request_id = self
                    .append_autonomy_budget_approval_request(
                        binding,
                        route,
                        event,
                        replay_of_event_id,
                        reason,
                    )
                    .await?;
                self.emit_autonomy_budget_approval_action_graph(
                    binding,
                    route,
                    event,
                    source_node_id.as_str(),
                    replay_of_event_id,
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
                    event,
                    replay_of_event_id,
                    autonomy_tier,
                    AutonomyTier::ActWithApproval,
                    reason,
                    &request_id,
                )
                .await?;
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
                return Ok(Some(DispatchOutcome {
                    trigger_id: binding.id.as_str().to_string(),
                    binding_key: binding.binding_key(),
                    event_id: event.id.0.clone(),
                    attempt_count: 0,
                    status: DispatchStatus::Waiting,
                    handler_kind: route.kind().to_string(),
                    target_uri: route.target_uri(),
                    replay_of_event_id: replay_of_event_id.cloned(),
                    result: Some(serde_json::json!({
                        "approval_required": true,
                        "request_id": request_id,
                        "reason": reason,
                        "reviewers": [DEFAULT_AUTONOMY_BUDGET_REVIEWER],
                    })),
                    error: None,
                }));
            }
            note_autonomous_decision(binding);
        }

        Ok(None)
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
}
