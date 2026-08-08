//! Dispatcher construction, inbox enqueue, and the run/drain/shutdown loop.
//!
//! Everything here is about the dispatcher as a long-lived object: building one
//! against an event log, appending events to the trigger inbox, subscribing to
//! that inbox and pumping envelopes into the routing layer, and winding the
//! whole thing down. Deciding what a single envelope means lives in `routing`.

use super::*;

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
}
