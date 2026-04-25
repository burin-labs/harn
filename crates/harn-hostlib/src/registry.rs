//! Registration plumbing.
//!
//! Each module exposes a [`HostlibCapability`] implementation that pushes
//! its builtins into a [`BuiltinRegistry`]. The registry can then either
//! be wired into a real [`harn_vm::Vm`] (production path) or introspected
//! by tests to assert the exposed surface without touching the VM.

use std::sync::Arc;

use harn_vm::{Vm, VmError, VmValue};

use crate::error::HostlibError;

/// Sync builtin handler signature. Mirrors the closure type accepted by
/// [`harn_vm::Vm::register_builtin`]; we keep it `Send + Sync` so capability
/// instances can be shared across threads if an embedder ever wants that.
pub type SyncHandler = Arc<dyn Fn(&[VmValue]) -> Result<VmValue, HostlibError> + Send + Sync>;

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

    /// Iterate over every registered builtin.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredBuiltin> {
        self.builtins.iter()
    }

    /// Total count.
    pub fn len(&self) -> usize {
        self.builtins.len()
    }

    /// True when nothing has been registered yet.
    pub fn is_empty(&self) -> bool {
        self.builtins.is_empty()
    }

    /// Look up a builtin by its Harn-visible name.
    pub fn find(&self, name: &str) -> Option<&RegisteredBuiltin> {
        self.builtins.iter().find(|b| b.name == name)
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
        for builtin in self.builtins.iter().cloned().collect::<Vec<_>>() {
            let handler = builtin.handler.clone();
            vm.register_builtin(
                builtin.name,
                move |args, _out| -> Result<VmValue, VmError> {
                    handler(args).map_err(VmError::from)
                },
            );
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
