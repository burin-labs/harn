//! Dispatching one attempt to whatever the binding\'s handler URI points at.
//!
//! `dispatch_once` is the single place that maps a resolved [`DispatchUri`] to
//! the machinery behind it — a local closure, an A2A peer, a worker queue, a
//! persona, an eval pack, an auto-resume target, a pool, a reminder injection,
//! or a panic broadcast. The three multi-step handlers with enough logic of
//! their own live in sibling modules (`spawn_to_pool`, `reminder_inject`,
//! `interrupt_and_suspend`, and the pre-existing `persona`).

use super::*;

impl Dispatcher {
    pub(super) async fn dispatch_once(
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
}
