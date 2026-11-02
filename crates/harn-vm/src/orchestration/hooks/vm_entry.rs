use super::*;

#[derive(Clone, Debug)]
pub struct VmLifecycleHookInvocation {
    pub(super) callable: crate::value::VmCallable,
    pub(super) tool_scope: crate::tool_handler_scope::CallbackScope,
    pub handler_name: String,
}

impl VmLifecycleHookInvocation {
    pub(crate) fn scope<F: std::future::Future>(
        &self,
        future: F,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = F::Output>>> {
        self.tool_scope.scope(future)
    }

    /// Resolve this invocation's handler closure against `vm`, loading the
    /// handler's module on demand for a lazy manifest hook (a cache hit
    /// when `vm` already imported the graph).
    pub async fn resolve(&self, vm: &mut crate::vm::Vm) -> Result<Arc<VmClosure>, VmError> {
        self.tool_scope
            .scope(vm.resolve_callable(&self.callable))
            .await
            .map_err(|error| {
                VmError::Runtime(format!(
                    "failed to resolve lifecycle hook '{}': {error}",
                    self.handler_name
                ))
            })
    }
}

#[derive(Clone, Debug)]
pub(super) struct VmLifecycleHookRegistration {
    pub(super) handler_name: String,
    pub(super) callable: crate::value::VmCallable,
    pub(super) tool_scope: crate::tool_handler_scope::CallbackScope,
}

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
    // Runtime callbacks retain the existing bridge-policy exemption. The raw
    // host-call restriction below still follows the registering tool scope.
    let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
    let (closure, tool_scope) = match handler {
        RuntimeHookHandler::Vm {
            callable,
            tool_scope,
            ..
        } => (
            tool_scope.scope(vm.resolve_callable(callable)).await?,
            *tool_scope,
        ),
        _ => return Ok(None),
    };
    let harness = vm.root_harness_value().ok_or_else(|| {
        VmError::Runtime(
            "runtime hook entrypoint requires Harness, but no root Harness is installed"
                .to_string(),
        )
    })?;
    let arg = crate::stdlib::json_to_vm_value(payload);
    let result = tool_scope
        .scope(vm.call_closure_pub(&closure, &[harness, arg]))
        .await;
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
        // Keep bridge policy and the captured raw host-call restriction active
        // across both lazy module resolution and the callback body.
        let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
        record_hook_call(&session_id, event, &registration.handler_name, payload);
        let closure = registration
            .tool_scope
            .scope(resolve_lifecycle_handler(&mut vm, &registration.callable))
            .await?;
        let raw = registration
            .tool_scope
            .scope(vm.call_closure_pub(&closure, &[harness.clone(), arg.clone()]))
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
