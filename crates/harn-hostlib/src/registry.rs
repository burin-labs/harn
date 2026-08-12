//! Registration plumbing.
//!
//! Each module exposes a [`HostlibCapability`] implementation that pushes
//! its builtins into a [`BuiltinRegistry`]. The registry can then either
//! be wired into a real [`harn_vm::Vm`] (production path) or introspected
//! by tests to assert the exposed surface without touching the VM.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_vm::{Vm, VmError, VmValue};

use crate::error::HostlibError;

fn capability_binding(
    module: &'static str,
    method: &'static str,
) -> (harn_builtin_meta::CapabilityId, &'static str) {
    harn_builtin_meta::host_capabilities::capability_binding_for_schema(module, method)
        .unwrap_or_else(|| panic!("hostlib schema `{module}.{method}` has no typed capability"))
}

/// Sync builtin handler signature. Mirrors the closure type accepted by
/// [`harn_vm::Vm::register_builtin`]; we keep it `Send + Sync` so capability
/// instances can be shared across threads if an embedder ever wants that.
pub type SyncHandler = Arc<dyn Fn(&[VmValue]) -> Result<VmValue, HostlibError> + Send + Sync>;
/// Async hostlib handler used by event-driven operations.
pub type AsyncHandler = Arc<
    dyn Fn(Vec<VmValue>) -> Pin<Box<dyn Future<Output = Result<VmValue, HostlibError>> + Send>>
        + Send
        + Sync,
>;

/// A dropped async host call must still interrupt the synchronous operation
/// that was moved to Tokio's blocking pool. `JoinHandle::abort` cannot stop a
/// blocking closure, so the owning VM's cancellation token is the explicit
/// handoff to the process wait loop.
struct CancelOnDrop(Option<Arc<AtomicBool>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.store(true, Ordering::SeqCst);
        }
    }
}

#[derive(Clone)]
/// One registered async builtin and its schema coordinates.
pub struct RegisteredAsyncBuiltin {
    /// Harn-visible builtin name.
    pub name: &'static str,
    /// Hostlib schema module.
    pub module: &'static str,
    /// Hostlib schema method.
    pub method: &'static str,
    /// Async implementation.
    pub handler: AsyncHandler,
}

/// One registered builtin. The name is what Harn scripts call (e.g.
/// `hostlib_ast_parse_file`); `module` and `method` are the canonical
/// schema-directory coordinates (`schemas/<module>/<method>.request.json`).
#[derive(Clone)]
pub struct RegisteredBuiltin {
    /// Builtin name as Harn scripts see it.
    pub name: &'static str,
    /// Module bucket (e.g. `"ast"`, `"tools"`).
    pub module: &'static str,
    /// Method name within the module (e.g. `"parse_file"`, `"search"`).
    pub method: &'static str,
    /// Handler invoked when Harn calls the builtin.
    pub handler: SyncHandler,
}

impl std::fmt::Debug for RegisteredBuiltin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredBuiltin")
            .field("name", &self.name)
            .field("module", &self.module)
            .field("method", &self.method)
            .finish()
    }
}

/// Mutable collector each capability writes into during `register`.
#[derive(Default)]
pub struct BuiltinRegistry {
    builtins: Vec<RegisteredBuiltin>,
    async_builtins: Vec<RegisteredAsyncBuiltin>,
    command_policy_builtins: BTreeSet<&'static str>,
}

impl BuiltinRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one builtin. Capabilities call this from `register_builtins`.
    pub fn register(&mut self, builtin: RegisteredBuiltin) {
        self.builtins.push(builtin);
    }

    /// Convenience: register a builtin whose body is the `unimplemented`
    /// scaffold error.
    pub fn register_unimplemented(
        &mut self,
        name: &'static str,
        module: &'static str,
        method: &'static str,
    ) {
        let handler: SyncHandler =
            Arc::new(move |_args| Err(HostlibError::Unimplemented { builtin: name }));
        self.register(RegisteredBuiltin {
            name,
            module,
            method,
            handler,
        });
    }

    /// Convenience: register a stateless builtin backed by a plain fn
    /// pointer. This is the shape almost every capability module uses;
    /// keeping it here avoids each module hand-rolling its own copy.
    pub(crate) fn register_fn(
        &mut self,
        module: &'static str,
        name: &'static str,
        method: &'static str,
        runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
    ) {
        let handler: SyncHandler = Arc::new(runner);
        self.register(RegisteredBuiltin {
            name,
            module,
            method,
            handler,
        });
    }

    /// Register a deterministic command-execution builtin whose request must
    /// cross the VM command-policy boundary before the hostlib handler runs.
    pub(crate) fn register_command_fn(
        &mut self,
        module: &'static str,
        name: &'static str,
        method: &'static str,
        runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
    ) {
        self.register_fn(module, name, method, runner);
        self.command_policy_builtins.insert(name);
    }

    pub(crate) fn register_async_fn<F, Fut>(
        &mut self,
        module: &'static str,
        name: &'static str,
        method: &'static str,
        runner: F,
    ) where
        F: Fn(Vec<VmValue>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<VmValue, HostlibError>> + Send + 'static,
    {
        let runner = Arc::new(runner);
        let handler: AsyncHandler = Arc::new(move |args| Box::pin(runner(args)));
        self.async_builtins.push(RegisteredAsyncBuiltin {
            name,
            module,
            method,
            handler,
        });
    }

    fn uses_command_policy(&self, name: &str) -> bool {
        self.command_policy_builtins.contains(name)
    }

    /// Iterate over every registered builtin.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredBuiltin> {
        self.builtins.iter()
    }

    /// Iterate over every registered async builtin.
    pub fn iter_async(&self) -> impl Iterator<Item = &RegisteredAsyncBuiltin> {
        self.async_builtins.iter()
    }

    /// Total count.
    pub fn len(&self) -> usize {
        self.builtins.len() + self.async_builtins.len()
    }

    /// True when nothing has been registered yet.
    pub fn is_empty(&self) -> bool {
        self.builtins.is_empty() && self.async_builtins.is_empty()
    }

    /// Look up a builtin by its Harn-visible name.
    pub fn find(&self, name: &str) -> Option<&RegisteredBuiltin> {
        self.builtins.iter().find(|b| b.name == name)
    }

    /// Look up one async builtin by its Harn-visible name.
    pub fn find_async(&self, name: &str) -> Option<&RegisteredAsyncBuiltin> {
        self.async_builtins.iter().find(|b| b.name == name)
    }
}

/// One module's worth of builtins. Kept tiny on purpose: capabilities exist
/// purely so tests can reason about the surface without booting a VM, and
/// so embedders can opt into individual modules.
pub trait HostlibCapability: 'static {
    /// Module name (matches the `schemas/<module>/` directory).
    fn module_name(&self) -> &'static str;

    /// Push every builtin this module exposes into `registry`.
    fn register_builtins(&self, registry: &mut BuiltinRegistry);
}

/// Composes capabilities and emits VM registrations.
///
/// `HostlibRegistry` is the type embedders interact with. It owns the
/// capability instances and the populated [`BuiltinRegistry`] together so
/// the same surface can be inspected by tests *and* wired into a VM.
pub struct HostlibRegistry {
    builtins: BuiltinRegistry,
    modules: Vec<&'static str>,
}

impl Default for HostlibRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HostlibRegistry {
    /// Construct an empty registry. Most callers want [`crate::install_default`]
    /// instead, which pre-populates every shipped capability.
    pub fn new() -> Self {
        Self {
            builtins: BuiltinRegistry::new(),
            modules: Vec::new(),
        }
    }

    /// Add one capability to the registry. Returns `self` for chaining.
    #[must_use]
    pub fn with<C: HostlibCapability>(mut self, capability: C) -> Self {
        let module = capability.module_name();
        capability.register_builtins(&mut self.builtins);
        self.modules.push(module);
        self
    }

    /// Wire every registered builtin into the supplied VM.
    pub fn register_into_vm(&mut self, vm: &mut Vm) {
        for builtin in self.builtins.iter().cloned() {
            let module = builtin.module;
            let method = builtin.method;
            let (capability, capability_method) = capability_binding(module, method);
            harn_vm::stdlib::host::register_callable_host_operation(
                module,
                method,
                "Hostlib schema-backed operation registered at runtime.",
            );
            let handler = builtin.handler.clone();
            if self.builtins.uses_command_policy(builtin.name) {
                let ambient_name = builtin.name;
                let policy_handler = Arc::new({
                    let handler = handler.clone();
                    move |ctx: harn_vm::AsyncBuiltinCtx,
                          args: Vec<VmValue>|
                          -> Pin<
                        Box<dyn Future<Output = Result<VmValue, VmError>> + Send>,
                    > {
                        let handler = handler.clone();
                        Box::pin(async move {
                            let request = crate::schemas::validate_request_args(
                                ambient_name,
                                module,
                                method,
                                &args,
                            )
                            .map_err(VmError::from)?;
                            let params = request.as_dict().ok_or_else(|| {
                                VmError::Runtime(format!(
                                    "{ambient_name}: validated request must be a dict"
                                ))
                            })?;
                            let caller = serde_json::json!({
                                "surface": "hostlib",
                                "builtin": ambient_name,
                                "module": module,
                                "method": method,
                                "session_id": harn_vm::current_agent_session_id(),
                            });
                            match harn_vm::orchestration::run_command_policy_preflight_with_ctx(
                                Some(&ctx),
                                params,
                                caller,
                            )
                            .await?
                            {
                                harn_vm::orchestration::CommandPolicyPreflight::Blocked {
                                    status,
                                    message,
                                    context,
                                    decisions,
                                } => {
                                    let response = harn_vm::orchestration::blocked_command_response(
                                        params, status, &message, context, decisions,
                                    );
                                    crate::schemas::validate_response(
                                        ambient_name,
                                        module,
                                        method,
                                        crate::tools::policy_blocked_run_command_response(response),
                                    )
                                    .map_err(VmError::from)
                                }
                                harn_vm::orchestration::CommandPolicyPreflight::Proceed {
                                    params,
                                    context,
                                    decisions,
                                } => {
                                    // Hooks may rewrite command fields. Revalidate
                                    // the rewritten request at the owning schema
                                    // boundary before the hostlib parser sees it.
                                    let rewritten = VmValue::dict(params.clone());
                                    let validated = crate::schemas::validate_request_args(
                                        ambient_name,
                                        module,
                                        method,
                                        &[rewritten],
                                    )
                                    .map_err(VmError::from)?;
                                    let (parent_cancel, deadline) = ctx.interrupt_sources();
                                    let cancel = parent_cancel.unwrap_or_else(|| {
                                        Arc::new(AtomicBool::new(false))
                                    });
                                    let mut cancel_on_drop = CancelOnDrop(Some(Arc::clone(&cancel)));
                                    let handler_for_blocking = handler.clone();
                                    let completed = harn_vm::orchestration::run_blocking_with_ambient(
                                        move || {
                                            let _interrupt = harn_vm::op_interrupt::install(
                                                Some(cancel),
                                                deadline,
                                            );
                                            handler_for_blocking(&[validated]).map_err(VmError::from)
                                        },
                                    )
                                    .await;
                                    // The blocking operation is no longer running, even when it
                                    // completed with a normal host error (for example ENOENT).
                                    // Disarm before propagating either layer of Result; otherwise
                                    // error unwinding drops the guard and poisons the parent VM's
                                    // shared cancellation token.
                                    cancel_on_drop.0 = None;
                                    let result = completed
                                    .map_err(|error| {
                                        VmError::Runtime(format!(
                                            "{ambient_name} blocking host operation failed: {error}"
                                        ))
                                    })??;
                                    if crate::tools::run_command_request_is_background(&params) {
                                        return crate::schemas::validate_response(
                                            ambient_name,
                                            module,
                                            method,
                                            result,
                                        )
                                        .map_err(VmError::from);
                                    }
                                    let result =
                                    harn_vm::orchestration::run_command_policy_postflight_with_ctx(
                                        Some(&ctx),
                                        &params,
                                        result,
                                        context,
                                        decisions,
                                    )
                                    .await?;
                                    crate::schemas::validate_response(
                                        ambient_name,
                                        module,
                                        method,
                                        result,
                                    )
                                    .map_err(VmError::from)
                                }
                            }
                        })
                    }
                });
                let capability_dispatch = Arc::clone(&policy_handler);
                vm.register_async_capability_method(
                    capability,
                    capability_method,
                    move |ctx, args| capability_dispatch(ctx, args),
                );
                // Legacy ambient wire name (`hostlib_tools_run_command`, …).
                // Keep the typed capability as the sole semantic owner; this
                // only re-exposes the pre-cutover global call shape.
                if harn_parser::legacy_ambient_capabilities_enabled() {
                    let ambient_dispatch = Arc::clone(&policy_handler);
                    vm.register_async_builtin(ambient_name, move |ctx, args| {
                        ambient_dispatch(ctx, args)
                    });
                }
            } else {
                let ambient_name = builtin.name;
                let sync_handler = Arc::new({
                    let handler = handler.clone();
                    move |args: &[VmValue], _out: &mut String| -> Result<VmValue, VmError> {
                        let request = crate::schemas::validate_request_args(
                            ambient_name,
                            module,
                            method,
                            args,
                        )
                        .map_err(VmError::from)?;
                        let validated_args = [request];
                        handler(&validated_args).map_err(VmError::from)
                    }
                });
                let capability_dispatch = Arc::clone(&sync_handler);
                vm.register_capability_method(capability, capability_method, move |args, out| {
                    capability_dispatch(args, out)
                });
                if harn_parser::legacy_ambient_capabilities_enabled() {
                    let ambient_dispatch = Arc::clone(&sync_handler);
                    vm.register_builtin(ambient_name, move |args, out| ambient_dispatch(args, out));
                }
            }
        }
        for builtin in self.builtins.async_builtins.iter().cloned() {
            let module = builtin.module;
            let method = builtin.method;
            let (capability, capability_method) = capability_binding(module, method);
            harn_vm::stdlib::host::register_callable_host_operation(
                module,
                method,
                "Hostlib schema-backed operation registered at runtime.",
            );
            let ambient_name = builtin.name;
            let handler = Arc::new({
                let handler = builtin.handler.clone();
                move |_ctx: harn_vm::AsyncBuiltinCtx,
                      args: Vec<VmValue>|
                      -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send>> {
                    let handler = handler.clone();
                    Box::pin(async move {
                        let request = crate::schemas::validate_request_args(
                            ambient_name,
                            module,
                            method,
                            &args,
                        )
                        .map_err(VmError::from)?;
                        let result = handler(vec![request]).await.map_err(VmError::from)?;
                        crate::schemas::validate_response(ambient_name, module, method, result)
                            .map_err(VmError::from)
                    })
                }
            });
            let capability_dispatch = Arc::clone(&handler);
            vm.register_async_capability_method(capability, capability_method, move |ctx, args| {
                capability_dispatch(ctx, args)
            });
            if harn_parser::legacy_ambient_capabilities_enabled() {
                let ambient_dispatch = Arc::clone(&handler);
                vm.register_async_builtin(ambient_name, move |ctx, args| {
                    ambient_dispatch(ctx, args)
                });
            }
        }
    }

    /// Borrow the underlying [`BuiltinRegistry`] for introspection (e.g.
    /// schema-drift tests).
    pub fn builtins(&self) -> &BuiltinRegistry {
        &self.builtins
    }

    /// List the module names that have been registered, in insertion order.
    pub fn modules(&self) -> &[&'static str] {
        &self.modules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_bridge_projects_legacy_hostlib_wire_names() {
        // Process-global env; keep this test self-contained and restore after.
        let previous = std::env::var_os(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV);
        // SAFETY: single-threaded unit test restoring the prior value.
        unsafe {
            std::env::remove_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV);
        }
        let mut strict_vm = Vm::new();
        crate::install_default(&mut strict_vm);
        assert!(
            strict_vm
                .builtin_metadata_for("hostlib_tools_run_command")
                .is_none(),
            "strict install must keep hostlib wire names off the ambient map"
        );

        unsafe {
            std::env::set_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV, "1");
        }
        let mut ambient_vm = Vm::new();
        crate::install_default(&mut ambient_vm);
        assert!(
            ambient_vm
                .builtin_metadata_for("hostlib_tools_run_command")
                .is_some(),
            "ambient bridge must project hostlib wire names as globals"
        );

        unsafe {
            match previous {
                Some(value) => {
                    std::env::set_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV, value);
                }
                None => std::env::remove_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV),
            }
        }
    }

    #[test]
    fn unimplemented_builtins_route_through_error() {
        let mut registry = BuiltinRegistry::new();
        registry.register_unimplemented("hostlib_demo", "demo", "ping");
        let entry = registry.find("hostlib_demo").expect("registered");
        let err = (entry.handler)(&[]).expect_err("should be unimplemented");
        assert!(
            matches!(err, HostlibError::Unimplemented { builtin } if builtin == "hostlib_demo")
        );
    }

    #[test]
    fn registry_records_modules_in_order() {
        struct First;
        impl HostlibCapability for First {
            fn module_name(&self) -> &'static str {
                "first"
            }
            fn register_builtins(&self, _registry: &mut BuiltinRegistry) {}
        }
        struct Second;
        impl HostlibCapability for Second {
            fn module_name(&self) -> &'static str {
                "second"
            }
            fn register_builtins(&self, _registry: &mut BuiltinRegistry) {}
        }

        let registry = HostlibRegistry::new().with(First).with(Second);
        assert_eq!(registry.modules(), &["first", "second"]);
    }
}
