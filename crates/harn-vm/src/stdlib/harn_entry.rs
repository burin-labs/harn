//! Registration helpers for public builtins implemented by Harn stdlib modules.

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HarnEntrypointModule {
    pub import_path: &'static str,
    pub category: &'static str,
}

impl HarnEntrypointModule {
    pub(crate) const fn new(import_path: &'static str, category: &'static str) -> Self {
        Self {
            import_path,
            category,
        }
    }
}

pub(crate) fn register_harn_module_entrypoints(vm: &mut Vm, modules: &[HarnEntrypointModule]) {
    for module in modules {
        let Some(module_name) = module.import_path.strip_prefix("std/") else {
            continue;
        };
        for export in harn_stdlib::public_functions_for_module(module_name) {
            let arity = arity_for_export(&export);
            let entrypoint = HarnEntrypoint {
                public_name: export.name.clone(),
                import_path: module.import_path.to_string(),
                export_name: export.name,
                signature: export.signature,
                arity,
                category: module.category.to_string(),
                doc: export.doc,
            };
            entrypoint.register(vm);
        }
    }
}

fn arity_for_export(export: &harn_stdlib::StdlibPublicFunction) -> VmBuiltinArity {
    if export.variadic {
        VmBuiltinArity::Variadic
    } else if export.required_params == export.total_params {
        VmBuiltinArity::Exact(export.total_params)
    } else {
        VmBuiltinArity::Range {
            min: export.required_params,
            max: export.total_params,
        }
    }
}

#[derive(Clone, Debug)]
struct HarnEntrypoint {
    public_name: String,
    import_path: String,
    export_name: String,
    signature: String,
    arity: VmBuiltinArity,
    category: String,
    doc: Option<String>,
}

impl HarnEntrypoint {
    fn register(self, vm: &mut Vm) {
        vm.register_async_builtin_with_metadata(self.metadata(), move |args| {
            let entrypoint = self.clone();
            Box::pin(async move { call_harn_export(entrypoint, args).await })
        });
    }

    fn metadata(&self) -> VmBuiltinMetadata {
        let mut metadata = VmBuiltinMetadata::async_builtin(self.public_name.clone())
            .signature_owned(self.signature.clone())
            .arity(self.arity)
            .category_owned(self.category.clone());
        if let Some(doc) = self.doc.clone() {
            metadata = metadata.doc_owned(doc);
        }
        metadata
    }
}

async fn call_harn_export(
    entrypoint: HarnEntrypoint,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(format!(
            "{}: Harn stdlib dispatch requires an async VM context",
            entrypoint.public_name
        ))
    })?;
    let exports = vm
        .load_module_exports_from_import(&entrypoint.import_path)
        .await?;
    let closure = exports
        .get(&entrypoint.export_name)
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
