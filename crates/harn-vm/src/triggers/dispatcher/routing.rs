//! Turning an inbox envelope or a bare event into concrete per-binding dispatches.
//!
//! This layer resolves which binding(s) an event belongs to — an explicitly
//! targeted trigger id, a cron payload naming its own trigger, or every binding
//! whose match rules accept the event — and wraps each resulting dispatch in the
//! tracing span, OpenTelemetry attributes, and dispatch metrics. The per-binding
//! work itself is `attempt_loop::dispatch_with_replay_inner`.

use super::*;

impl Dispatcher {
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

    pub(super) async fn dispatch_inbox_envelope_with_headers(
        &self,
        envelope: InboxEnvelope,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        if let Some(trigger_id) = envelope.trigger_id {
            let binding = crate::triggers::registry::resolve_live_trigger_binding(
                &trigger_id,
                envelope.binding_version,
            )
            .map_err(|error| DispatchError::Registry(error.to_string()))?;
            // Cron names a specific scheduled invocation, rather than a
            // connector delivery that event subscriptions can consume.
            if envelope.event.provider.as_str() == "cron" {
                return Ok(vec![
                    self.dispatch_with_replay(&binding, envelope.event, None, None, parent_headers)
                        .await?,
                ]);
            }
            let mut bindings = matching_bindings(&envelope.event)
                .into_iter()
                .filter(|candidate| candidate.ingress_subscription && candidate.id != binding.id)
                .collect::<Vec<_>>();
            let skipped = if binding.matches_event(&envelope.event) {
                bindings.insert(0, binding);
                None
            } else {
                Some(self.skip_ingress_binding(&binding, &envelope.event,
                    DispatchSkipStage::IngressSelector,
                    serde_json::json!({"provider": envelope.event.provider.as_str(), "kind": envelope.event.kind, "match_events": binding.match_events}),
                ).await?)
            };
            let mut outcomes = self
                .dispatch_ingress_bindings(bindings, envelope.event, parent_headers)
                .await?;
            if let Some(skipped) = skipped {
                outcomes.insert(0, skipped);
            }
            return Ok(outcomes);
        }

        let cron_target = match &envelope.event.provider_payload {
            crate::triggers::ProviderPayload::Known(
                crate::triggers::event::KnownProviderPayload::Cron(payload),
            ) => payload.cron_id.clone(),
            _ => None,
        };
        if let Some(trigger_id) = cron_target {
            let binding = crate::triggers::registry::resolve_live_trigger_binding(
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

    async fn dispatch_ingress_bindings(
        &self,
        bindings: Vec<Arc<TriggerBinding>>,
        event: TriggerEvent,
        parent_headers: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<DispatchOutcome>, DispatchError> {
        let mut outcomes = Vec::new();
        let mut first_error = None;
        for binding in bindings {
            let result = async {
                let duplicate = if binding.ingress_subscription {
                    let mut inboxes = self.ingress_inboxes.lock().await;
                    let tenant_key = event.tenant_id.as_ref().map(|tenant| tenant.0.clone());
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        inboxes.entry(tenant_key.clone())
                    {
                        let metrics = self.metrics.clone().unwrap_or_default();
                        let inbox = match event.tenant_id.as_ref() {
                            Some(tenant) => {
                                crate::triggers::InboxIndex::new_for_tenant(
                                    self.event_log.clone(),
                                    metrics,
                                    tenant,
                                )
                                .await
                            }
                            None => {
                                crate::triggers::InboxIndex::new(self.event_log.clone(), metrics)
                                    .await
                            }
                        }
                        .map_err(|error| DispatchError::EventLog(error.to_string()))?;
                        entry.insert(inbox);
                    }
                    // A route's dedupe_claimed flag belongs to that route only.
                    // Subscriptions claim independently, using the connector's
                    // delivery key across routes and binding versions.
                    let is_new = inboxes[&tenant_key]
                        .insert_if_new(
                            binding.id.as_str(),
                            &event.dedupe_key,
                            Duration::from_secs(
                                u64::from(binding.dedupe_retention_days.max(1)) * 86400,
                            ),
                        )
                        .await
                        .map_err(|error| DispatchError::EventLog(error.to_string()))?;
                    !is_new
                } else {
                    false
                };
                if duplicate {
                    return self
                        .skip_ingress_binding(
                            &binding,
                            &event,
                            DispatchSkipStage::InboxDedupe,
                            serde_json::json!({"deduplicated": true}),
                        )
                        .await;
                }
                self.dispatch_with_replay(&binding, event.clone(), None, None, parent_headers)
                    .await
            }
            .await;
            match result {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(outcomes),
        }
    }

    async fn skip_ingress_binding(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        stage: DispatchSkipStage,
        detail: serde_json::Value,
    ) -> Result<DispatchOutcome, DispatchError> {
        let route = DispatchUri::from(&binding.handler);
        self.append_skipped_outbox_event(
            binding,
            &route,
            event,
            None,
            stage.clone(),
            detail.clone(),
        )
        .await?;
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.clear_trigger_pending_event(
                &event.id.0,
                binding.id.as_str(),
                &binding.binding_key(),
                event.provider.as_str(),
                tenant_id(event),
                current_unix_ms(),
            );
        }
        Ok(DispatchOutcome {
            trigger_id: binding.id.as_str().to_string(),
            binding_key: binding.binding_key(),
            event_id: event.id.0.clone(),
            attempt_count: 0,
            status: DispatchStatus::Skipped,
            handler_kind: route.kind().to_string(),
            target_uri: route.target_uri(),
            replay_of_event_id: None,
            result: Some(
                serde_json::json!({"skipped": true, "skip_stage": stage.as_str(), "detail": detail}),
            ),
            error: None,
        })
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
        // The attempt loop retains the event, action graph, receipts, leases,
        // retry state, and handler future. Keep that large state machine on
        // the heap before wrapping it in tracing and task-local scopes; those
        // generic wrappers otherwise embed it repeatedly in every caller up
        // through MCP and other synchronous host entrypoints.
        let dispatch = Box::pin(
            self.dispatch_with_replay_inner(
                binding,
                event,
                replay_of_event_id.clone(),
                parent_headers_for_metrics,
            )
            .instrument(span),
        );
        let outcome = ACTIVE_DISPATCH_IS_REPLAY
            .scope(replay_of_event_id.is_some(), dispatch)
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
}
