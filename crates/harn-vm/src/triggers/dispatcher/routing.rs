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
}
