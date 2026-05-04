//! Compact registration DSL for low-level stdlib host primitives.

use std::future::Future;
use std::pin::Pin;

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

pub(crate) type AsyncBuiltinFuture = Pin<Box<dyn Future<Output = Result<VmValue, VmError>>>>;
pub(crate) type SyncBuiltinHandler = fn(&[VmValue], &mut String) -> Result<VmValue, VmError>;
pub(crate) type AsyncBuiltinHandler = fn(Vec<VmValue>) -> AsyncBuiltinFuture;

#[derive(Clone)]
pub(crate) struct SyncBuiltin {
    name: &'static str,
    signature: Option<&'static str>,
    arity: Option<VmBuiltinArity>,
    doc: Option<&'static str>,
    handler: SyncBuiltinHandler,
}

impl SyncBuiltin {
    pub(crate) const fn new(name: &'static str, handler: SyncBuiltinHandler) -> Self {
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

    fn metadata(&self, default_category: Option<&'static str>) -> VmBuiltinMetadata {
        let mut metadata = VmBuiltinMetadata::sync_static(self.name);
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

#[derive(Clone)]
pub(crate) struct AsyncBuiltin {
    name: &'static str,
    signature: Option<&'static str>,
    arity: Option<VmBuiltinArity>,
    doc: Option<&'static str>,
    handler: AsyncBuiltinHandler,
}

impl AsyncBuiltin {
    pub(crate) const fn new(name: &'static str, handler: AsyncBuiltinHandler) -> Self {
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

    fn metadata(&self, default_category: Option<&'static str>) -> VmBuiltinMetadata {
        let mut metadata = VmBuiltinMetadata::async_static(self.name);
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

pub(crate) fn boxed_async_builtin<Fut>(future: Fut) -> AsyncBuiltinFuture
where
    Fut: Future<Output = Result<VmValue, VmError>> + 'static,
{
    Box::pin(future)
}

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
