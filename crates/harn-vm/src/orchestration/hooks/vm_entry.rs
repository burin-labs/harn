use super::*;

/// Invoke a VM-backed hook handler against a child of the firing VM.
pub(super) async fn invoke_vm_hook_handler(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    handler: &RuntimeHookHandler,
    payload: &serde_json::Value,
) -> Result<Option<VmValue>, VmError> {
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    // First-party registered hook (`register_session_hook` /
    // `register_checkpoint_hook`): the runtime chose to invoke this closure,
    // so its body's bridge/builtin calls are a trusted bridge call and must
    // not trip the agent loop's active execution policy. Held across the await.
    let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
    let closure = match handler {
        RuntimeHookHandler::Vm { callable, .. } => vm.resolve_callable(callable).await?,
        _ => return Ok(None),
    };
    let harness = vm.root_harness_value().ok_or_else(|| {
        VmError::Runtime(
            "runtime hook entrypoint requires Harness, but no root Harness is installed"
                .to_string(),
        )
    })?;
    let arg = crate::stdlib::json_to_vm_value(payload);
    let result = vm.call_closure_pub(&closure, &[harness, arg]).await;
    if let Some(ctx) = ctx {
        ctx.forward_output(&vm.take_output());
    }
    Ok(Some(result?))
}

pub(super) async fn invoke_vm_lifecycle_hooks(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: HookEvent,
    registrations: Vec<VmLifecycleHookRegistration>,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    let harness = vm.root_harness_value().ok_or_else(|| {
        VmError::Runtime(
            "lifecycle hook entrypoint requires Harness, but no root Harness is installed"
                .to_string(),
        )
    })?;
    let arg = crate::stdlib::json_to_vm_value(payload);
    let session_id = payload
        .get("session")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    for registration in registrations {
        // First-party registered lifecycle hook: the runtime chose to invoke
        // this closure, so its body's bridge/builtin calls are a trusted bridge
        // call and must not trip the agent loop's active execution policy. Held
        // across resolution and the invocation await for this registration.
        let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
        record_hook_call(&session_id, event, &registration.handler_name, payload);
        let closure = resolve_lifecycle_handler(&mut vm, &registration.callable).await?;
        let raw = vm
            .call_closure_pub(&closure, &[harness.clone(), arg.clone()])
            .await?;
        if let Some(ctx) = ctx {
            ctx.forward_output(&vm.take_output());
        }
        let effects = parse_hook_effects(event, &raw)?;
        record_hook_returned(
            &session_id,
            event,
            &registration.handler_name,
            &HookControl::Allow,
            &raw,
        );
        inject_hook_effects(session_id.as_str(), effects, Some(event))?;
    }
    Ok(())
}
