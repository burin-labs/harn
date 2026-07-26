//! Applying a binding's flow-control configuration to one dispatch.
//!
//! Resolves each gate's key expression against the event, then drives the
//! [`flow_control::FlowControlManager`] primitives in order — batch, debounce,
//! rate limit, throttle, singleton, concurrency — and releases the singleton and
//! concurrency leases when the dispatch finishes. The manager owns the queues
//! and windows; this module owns the per-binding policy that consults them.

use super::*;

impl Dispatcher {
    pub(super) async fn apply_flow_control(
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

    pub(super) async fn release_flow_control(
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
