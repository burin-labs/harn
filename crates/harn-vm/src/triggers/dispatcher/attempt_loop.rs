//! The per-binding dispatch state machine: one event, one binding, one verdict.
//!
//! Seeds the action graph for the event, runs the admission gates, acquires the
//! flow-control leases and checks the destination circuit, then loops over
//! attempts — emitting lifecycle/outbox/action-graph records for each one and
//! deciding between success, waiting, retry-with-backoff, and the dead-letter
//! queue. Every exit path releases the leases it took and settles the binding\'s
//! in-flight accounting.

use super::*;

impl Dispatcher {
    pub(super) async fn dispatch_with_replay_inner(
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
            crate::triggers::registry::begin_replay_in_flight(binding.id.as_str(), binding.version)
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

        if let Some(outcome) = self
            .run_admission_gates(
                binding,
                &route,
                &event,
                replay_of_event_id.as_ref(),
                autonomy_tier,
                &mut source_node_id,
            )
            .await?
        {
            return Ok(outcome);
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
}
