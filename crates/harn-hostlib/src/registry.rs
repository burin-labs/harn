//! Registration plumbing.
//!
//! Each module exposes a [`HostlibCapability`] implementation that pushes
//! its builtins into a [`BuiltinRegistry`]. The registry can then either
//! be wired into a real [`harn_vm::Vm`] (production path) or introspected
//! by tests to assert the exposed surface without touching the VM.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_vm::{Vm, VmError, VmValue};

use crate::error::HostlibError;
use crate::host_call::{CallAddress, HostCallContext};

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

pub(crate) struct PreparedCommand {
    pub(crate) params: harn_vm::value::DictMap,
    pub(crate) invocation: harn_vm::orchestration::CommandInvocation,
    pub(crate) handler: SyncHandler,
}

impl harn_vm::orchestration::CommandPolicyParameters for PreparedCommand {
    fn parameters(&self) -> &harn_vm::value::DictMap {
        &self.params
    }

    fn invocation(&self) -> Option<serde_json::Value> {
        Some(self.invocation.audit())
    }
}

type CommandPreparer = fn(harn_vm::value::DictMap) -> Result<PreparedCommand, HostlibError>;
/// Async hostlib handler used by event-driven operations.
pub type AsyncHandler = Arc<
    dyn Fn(Vec<VmValue>) -> Pin<Box<dyn Future<Output = Result<VmValue, HostlibError>> + Send>>
        + Send
        + Sync,
>;

/// A dropped async host call must still interrupt the synchronous operation
/// that was moved to Tokio's blocking pool. `JoinHandle::abort` cannot stop a
/// blocking closure, so per-call cancellation retires its enrolled processes
/// before the async caller can report a terminal result.
struct CancelOnDrop {
    scope: crate::process::call_scope::CallProcessScope,
    _execution_cleanup: Option<harn_vm::op_interrupt::ActiveProcessCleanupGuard>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self.scope.cancel_and_drain();
    }
}

/// Run a validated sync handler on Tokio's blocking pool, so the VM's thread
/// keeps delivering cancellation and progress while the handler waits.
///
/// The worker observes the VM's deadlines and a token of its own that the VM
/// trips by dropping the call: a tool-call cancel, a VM cancel, or an expired
/// deadline. Parent cancellation is also observed, while stopping one call
/// leaves the VM running. The worker also gets the [`HostCallContext`] of the tool call
/// that dispatched it.
async fn run_interruptible(
    ctx: &harn_vm::AsyncBuiltinCtx,
    builtin: &'static str,
    handler: SyncHandler,
    request: VmValue,
) -> Result<VmValue, VmError> {
    let mut sources = ctx.interrupt_sources();
    let parent_cancel = sources.cancel.clone();
    let cancelled = sources
        .cancel
        .as_ref()
        .is_some_and(|token| token.load(Ordering::SeqCst));
    let cancel = Arc::new(AtomicBool::new(cancelled));
    sources.cancel = Some(Arc::clone(&cancel));
    let process_scope = crate::process::call_scope::CallProcessScope::new(cancel, parent_cancel)
        .with_execution_cancel(ctx.execution_cancel_token());
    let execution_cleanup = ctx.execution_cancel_token().map(|owner| {
        let guard = harn_vm::op_interrupt::register_active_process_cleanup_scope(
            owner.clone(),
            Arc::new(process_scope.clone()),
        );
        if owner.load(Ordering::SeqCst) {
            let _ = process_scope.cancel_and_drain();
        }
        guard
    });
    let address = CallAddress::current();
    let call = HostCallContext::new(address.clone()).with_process_scope(process_scope.clone());
    let completion = call.clone();
    let _cancel_on_drop = CancelOnDrop {
        scope: process_scope,
        _execution_cleanup: execution_cleanup,
    };
    let result = harn_vm::orchestration::run_blocking_with_ambient(move || {
        let _interrupt = sources.install();
        let _session = address
            .as_ref()
            .map(|call| harn_vm::agent_sessions::enter_current_session(call.session_id.clone()));
        let _tool_call = address.as_ref().map(|call| {
            harn_vm::agent_sessions::enter_current_tool_call(call.tool_call_id.clone())
        });
        let _call = call.enter();
        handler(&[request]).map_err(VmError::from)
    })
    .await
    .map_err(|error| {
        VmError::Runtime(format!("{builtin} blocking host operation failed: {error}"))
    })?;
    if completion.tool_call_cancelled() {
        return Err(VmError::ToolCallStopped);
    }
    result
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
    command_policy_builtins: BTreeMap<&'static str, CommandPreparer>,
    interruptible_builtins: BTreeSet<&'static str>,
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

    /// [`register_fn`](Self::register_fn) for a builtin that writes files: it
    /// holds the workspace write lease for its whole run, so its writes never
    /// interleave with those of a builtin running off the VM's thread.
    pub(crate) fn register_writer_fn(
        &mut self,
        module: &'static str,
        name: &'static str,
        method: &'static str,
        runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
    ) {
        let handler: SyncHandler = Arc::new(move |args| {
            let _writes = crate::host_call::workspace_writes();
            runner(args)
        });
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
        prepare: CommandPreparer,
    ) {
        self.register_fn(module, name, method, runner);
        self.command_policy_builtins.insert(name, prepare);
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

    /// Register a builtin that can wait on an external process. It runs off
    /// the VM's thread under a [`HostCallContext`], so its caller can stop it
    /// and see its progress while it waits.
    pub(crate) fn register_interruptible(&mut self, builtin: RegisteredBuiltin) {
        self.interruptible_builtins.insert(builtin.name);
        self.register(builtin);
    }

    /// [`register_interruptible`](Self::register_interruptible) for a plain
    /// handler function.
    pub(crate) fn register_interruptible_fn(
        &mut self,
        module: &'static str,
        name: &'static str,
        method: &'static str,
        runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
    ) {
        let handler: SyncHandler = Arc::new(runner);
        self.register_interruptible(RegisteredBuiltin {
            name,
            module,
            method,
            handler,
        });
    }

    fn command_preparer(&self, name: &str) -> Option<CommandPreparer> {
        self.command_policy_builtins.get(name).copied()
    }

    fn is_interruptible(&self, name: &str) -> bool {
        self.interruptible_builtins.contains(name)
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
            if let Some(prepare) = self.builtins.command_preparer(builtin.name) {
                let ambient_name = builtin.name;
                let policy_handler = Arc::new({
                    move |ctx: harn_vm::AsyncBuiltinCtx,
                          args: Vec<VmValue>|
                          -> Pin<
                        Box<dyn Future<Output = Result<VmValue, VmError>> + Send>,
                    > {
                        let background = args
                            .first()
                            .and_then(VmValue::as_dict)
                            .is_some_and(crate::tools::run_command_request_is_detached);
                        Box::pin(async move {
                            let taped_args = args.clone();
                            let live = Box::pin(async move {
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
                            match harn_vm::orchestration::run_prepared_command_policy_preflight(
                                Some(&ctx),
                                params,
                                caller,
                                harn_vm::orchestration::CommandDispatchOrigin::ArbitraryProcess,
                                |params| async move {
                                    let validated = crate::schemas::validate_request_args(
                                        ambient_name, module, method, &[VmValue::dict(params)],
                                    ).map_err(VmError::from)?;
                                    prepare(validated.as_dict().expect("validated command dict").clone())
                                        .map_err(VmError::from)
                                },
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
                                    let PreparedCommand { params, handler, .. } = params;
                                    let result = run_interruptible(
                                        &ctx,
                                        ambient_name,
                                        handler,
                                        VmValue::dict(params.clone()),
                                    )
                                    .await?;
                                    if crate::tools::run_command_response_is_running(&result) {
                                        harn_vm::run_tape::note_command_outlived_dispatch();
                                    }
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
                            });
                            harn_vm::run_tape::taped_command_run(ambient_name, &taped_args, background, live).await
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
            } else if self.builtins.is_interruptible(builtin.name) {
                let ambient_name = builtin.name;
                let interruptible_handler = Arc::new({
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
                            run_interruptible(&ctx, ambient_name, handler, request).await
                        })
                    }
                });
                let capability_dispatch = Arc::clone(&interruptible_handler);
                vm.register_async_capability_method(
                    capability,
                    capability_method,
                    move |ctx, args| capability_dispatch(ctx, args),
                );
                if harn_parser::legacy_ambient_capabilities_enabled() {
                    let ambient_dispatch = Arc::clone(&interruptible_handler);
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
#[path = "registry_run_tape_tests.rs"]
mod run_tape_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn interruptible_calls_keep_the_execution_cancel_source_when_the_child_token_differs() {
        let execution_cancel = Arc::new(AtomicBool::new(false));
        let child_cancel = Arc::new(AtomicBool::new(false));
        let mut parent = harn_vm::new_vm();
        parent.install_cancel_token(Arc::clone(&execution_cancel));
        let mut child = parent.child_vm_for_host();
        child.install_cancel_token(Arc::clone(&child_cancel));
        let ctx = harn_vm::AsyncBuiltinCtx::from_vm(child);
        assert!(Arc::ptr_eq(
            &ctx.execution_cancel_token().unwrap(),
            &execution_cancel,
        ));
        let handler: SyncHandler = Arc::new(move |_| {
            execution_cancel.store(true, Ordering::SeqCst);
            assert!(!child_cancel.load(Ordering::SeqCst));
            let call = HostCallContext::current();
            assert!(
                call.process_scope().unwrap().cancel_requested(),
                "a child host call lost its session cancellation source"
            );
            Ok(VmValue::Nil)
        });
        run_interruptible(&ctx, "execution_cancel_probe", handler, VmValue::Nil)
            .await
            .expect("the native worker must observe cancellation before VM propagation");
    }

    #[test]
    fn ambient_bridge_projects_legacy_hostlib_wire_names() {
        // Process-global env; keep this test self-contained and restore after.
        let previous = std::env::var_os(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV);
        // SAFETY: single-threaded unit test restoring the prior value.
        unsafe {
            std::env::remove_var(harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV);
        }
        harn_parser::refresh_legacy_ambient_capabilities();
        let mut strict_vm = harn_vm::new_vm();
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
        harn_parser::refresh_legacy_ambient_capabilities();
        let mut ambient_vm = harn_vm::new_vm();
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
        harn_parser::refresh_legacy_ambient_capabilities();
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
