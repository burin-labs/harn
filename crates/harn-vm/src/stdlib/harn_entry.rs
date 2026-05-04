//! Registration helpers for public builtins implemented by Harn stdlib modules.

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HarnAsyncEntrypoint {
    pub public_name: &'static str,
    pub import_path: &'static str,
    pub export_name: &'static str,
    signature: Option<&'static str>,
    arity: Option<VmBuiltinArity>,
    category: Option<&'static str>,
    doc: Option<&'static str>,
}

impl HarnAsyncEntrypoint {
    pub(crate) const fn new(
        public_name: &'static str,
        import_path: &'static str,
        export_name: &'static str,
    ) -> Self {
        Self {
            public_name,
            import_path,
            export_name,
            signature: None,
            arity: None,
            category: None,
            doc: None,
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

    pub(crate) const fn category(mut self, category: &'static str) -> Self {
        self.category = Some(category);
        self
    }

    pub(crate) const fn doc(mut self, doc: &'static str) -> Self {
        self.doc = Some(doc);
        self
    }

    fn register(self, vm: &mut Vm) {
        vm.register_async_builtin_with_metadata(self.metadata(), move |args| {
            Box::pin(async move { call_harn_export(self, args).await })
        });
    }

    fn metadata(self) -> VmBuiltinMetadata {
        let mut metadata = VmBuiltinMetadata::async_static(self.public_name);
        if let Some(signature) = self.signature {
            metadata = metadata.signature_static(signature);
        }
        if let Some(arity) = self.arity {
            metadata = metadata.arity(arity);
        }
        if let Some(category) = self.category {
            metadata = metadata.category_static(category);
        }
        if let Some(doc) = self.doc {
            metadata = metadata.doc_static(doc);
        }
        metadata
    }
}

pub(crate) fn register_harn_async_entrypoints(vm: &mut Vm, entrypoints: &[HarnAsyncEntrypoint]) {
    for entrypoint in entrypoints {
        entrypoint.register(vm);
    }
}

async fn call_harn_export(
    entrypoint: HarnAsyncEntrypoint,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(format!(
            "{}: Harn stdlib dispatch requires an async VM context",
            entrypoint.public_name
        ))
    })?;
    let exports = vm
        .load_module_exports_from_import(entrypoint.import_path)
        .await?;
    let closure = exports
        .get(entrypoint.export_name)
        .cloned()
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{}: stdlib module {} did not export `{}`",
                entrypoint.public_name, entrypoint.import_path, entrypoint.export_name
            ))
        })?;
    let result = vm.call_closure_pub(&closure, &args).await;
    let output = vm.take_output();
    crate::vm::forward_child_output_to_parent(&output);
    result
}
