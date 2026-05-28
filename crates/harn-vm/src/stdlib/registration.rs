//! Compact registration DSL for low-level stdlib host primitives.
//!
//! **Deprecated.** New stdlib builtins MUST be registered with the
//! `#[harn_builtin]` proc-macro (`crate::stdlib::macros::harn_builtin`). This
//! module survives only for a handful of unmigrated captured-state callers
//! (`runtime_context.rs`, `step_runtime.rs`, `checkpoint.rs`, `metadata.rs`,
//! and similar) whose handlers close over `Rc`/`Arc` state that the macro
//! doesn't yet model. Once those migrate this module will be removed.
//!
//! See `CONTRIBUTING.md` ("Adding a stdlib builtin") for the canonical
//! `#[harn_builtin]` template.

use std::future::Future;
use std::pin::Pin;

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

pub(crate) type AsyncBuiltinFuture = Pin<Box<dyn Future<Output = Result<VmValue, VmError>>>>;
pub(crate) type SyncBuiltinHandler = fn(&[VmValue], &mut String) -> Result<VmValue, VmError>;
pub(crate) type AsyncBuiltinHandler = fn(Vec<VmValue>) -> AsyncBuiltinFuture;

/// Generate a builder struct for a builtin kind (sync or async).
///
/// Both kinds share the same builder surface — name, optional signature,
/// arity, and doc — and the same metadata-application logic. The only
/// thing that differs is the handler type and which `VmBuiltinMetadata`
/// constructor stamps the kind (`sync_static` vs `async_static`).
macro_rules! builtin_kind {
    ($struct_name:ident, $handler_ty:ident, $meta_ctor:ident) => {
        #[derive(Clone)]
        pub(crate) struct $struct_name {
            name: &'static str,
            signature: Option<&'static str>,
            arity: Option<VmBuiltinArity>,
            doc: Option<&'static str>,
            handler: $handler_ty,
        }

        impl $struct_name {
            pub(crate) const fn new(name: &'static str, handler: $handler_ty) -> Self {
                Self {
                    name,
                    signature: None,
                    arity: None,
                    doc: None,
                    handler,
                }
            }

            pub(crate) const fn signature(mut self, signature: &'static str) -> Self {
                self.signature = Some(signature);
                self
            }

            pub(crate) const fn arity(mut self, arity: VmBuiltinArity) -> Self {
                self.arity = Some(arity);
                self
            }

            pub(crate) const fn doc(mut self, doc: &'static str) -> Self {
                self.doc = Some(doc);
                self
            }

            pub(crate) const fn name(&self) -> &'static str {
                self.name
            }

            fn metadata(&self, default_category: Option<&'static str>) -> VmBuiltinMetadata {
                let mut metadata = VmBuiltinMetadata::$meta_ctor(self.name);
                if let Some(signature) = self.signature {
                    metadata = metadata.signature_static(signature);
                }
                if let Some(arity) = self.arity {
                    metadata = metadata.arity(arity);
                }
                if let Some(category) = default_category {
                    metadata = metadata.category_static(category);
                }
                if let Some(doc) = self.doc {
                    metadata = metadata.doc_static(doc);
                }
                metadata
            }
        }
    };
}

builtin_kind!(SyncBuiltin, SyncBuiltinHandler, sync_static);
builtin_kind!(AsyncBuiltin, AsyncBuiltinHandler, async_static);

pub(crate) fn boxed_async_builtin<Fut>(future: Fut) -> AsyncBuiltinFuture
where
    Fut: Future<Output = Result<VmValue, VmError>> + 'static,
{
    Box::pin(future)
}

macro_rules! async_builtin {
    ($name:expr, $handler:path) => {
        $crate::stdlib::registration::AsyncBuiltin::new($name, |args| {
            $crate::stdlib::registration::boxed_async_builtin($handler(args))
        })
    };
}

pub(crate) use async_builtin;

#[derive(Clone, Copy)]
pub(crate) struct BuiltinGroup<'a> {
    category: Option<&'static str>,
    sync: &'a [SyncBuiltin],
    async_: &'a [AsyncBuiltin],
}

impl<'a> BuiltinGroup<'a> {
    pub(crate) const fn new() -> Self {
        Self {
            category: None,
            sync: &[],
            async_: &[],
        }
    }

    pub(crate) const fn category(mut self, category: &'static str) -> Self {
        self.category = Some(category);
        self
    }

    pub(crate) const fn sync(mut self, builtins: &'a [SyncBuiltin]) -> Self {
        self.sync = builtins;
        self
    }

    pub(crate) const fn async_(mut self, builtins: &'a [AsyncBuiltin]) -> Self {
        self.async_ = builtins;
        self
    }
}

pub(crate) fn register_builtin_group(vm: &mut Vm, group: BuiltinGroup<'_>) {
    for builtin in group.sync {
        vm.register_builtin_with_metadata(builtin.metadata(group.category), builtin.handler);
    }
    for builtin in group.async_ {
        vm.register_async_builtin_with_metadata(builtin.metadata(group.category), builtin.handler);
    }
}

pub(crate) fn register_builtin_groups(vm: &mut Vm, groups: &[BuiltinGroup<'_>]) {
    for group in groups {
        register_builtin_group(vm, *group);
    }
}

pub(crate) fn register_deferred_builtin_group(
    vm: &mut Vm,
    group: BuiltinGroup<'_>,
    registrar: fn(&mut Vm),
) {
    for builtin in group.sync {
        vm.register_deferred_builtin(builtin.name(), registrar);
    }
    for builtin in group.async_ {
        vm.register_deferred_builtin(builtin.name(), registrar);
    }
}

pub(crate) fn register_deferred_builtin_groups(
    vm: &mut Vm,
    groups: &[BuiltinGroup<'_>],
    registrar: fn(&mut Vm),
) {
    for group in groups {
        register_deferred_builtin_group(vm, *group, registrar);
    }
}
