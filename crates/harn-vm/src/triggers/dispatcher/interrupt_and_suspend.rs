//! The `InterruptAndSuspend` handler (CH-10 / #1910): resolve the target worker
//! scope and cooperatively suspend every worker in it.

use super::*;

impl Dispatcher {
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
    pub(super) async fn dispatch_interrupt_and_suspend(
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
}
