//! The `SpawnToPool` handler (#1889): run the binding\'s task factory and hand
//! the closure it returns to a named pool.

use super::*;

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_spawn_to_pool(
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
}
