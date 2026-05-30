//! Registration helpers for public builtins implemented by Harn stdlib modules.

use std::rc::Rc;

use serde::de::DeserializeOwned;

use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

pub(crate) fn register_harn_entrypoint_category(vm: &mut Vm, category: &str) {
    for module in harn_stdlib::entrypoint_modules() {
        if module.category != category {
            continue;
        }
        let Some(module_name) = module.import_path.strip_prefix("std/") else {
            continue;
        };
        for export in harn_stdlib::public_functions_for_module(module_name) {
            let arity = arity_for_export(&export);
            let entrypoint = HarnEntrypoint {
                public_name: export.name.clone(),
                import_path: module.import_path.clone(),
                export_name: export.name,
                signature: export.signature,
                arity,
                category: module.category.clone(),
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
        vm.register_async_builtin_with_metadata(self.metadata(), move |_ctx, args| {
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
    call_harn_export_by_name(
        &entrypoint.import_path,
        &entrypoint.export_name,
        &entrypoint.public_name,
        &args,
    )
    .await
}

pub(crate) async fn call_harn_export_by_name(
    import_path: &str,
    export_name: &str,
    label: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(format!(
            "{label}: Harn stdlib dispatch requires an async VM context"
        ))
    })?;
    let saved_env = std::mem::take(&mut vm.env);
    let saved_imported_paths = std::mem::take(&mut vm.imported_paths);
    let saved_source_dir = vm.source_dir.clone();
    let exports = vm.load_module_exports_from_import(import_path).await;
    vm.env = saved_env;
    vm.imported_paths = saved_imported_paths;
    vm.source_dir = saved_source_dir;
    let exports = exports?;
    let closure = exports.get(export_name).cloned().ok_or_else(|| {
        VmError::Runtime(format!(
            "{label}: stdlib module {import_path} did not export `{export_name}`"
        ))
    })?;
    let result = vm.call_closure_pub(&closure, args).await;
    let output = vm.take_output();
    crate::vm::forward_child_output_to_parent(&output);
    result
}

pub(crate) async fn call_agent_loop(
    prompt: String,
    system: Option<String>,
    options: std::collections::BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    call_harn_export_by_name(
        "std/agent/loop",
        "agent_loop",
        "workflow_stage_agent_loop",
        &[
            VmValue::String(Rc::from(prompt)),
            system
                .map(|value| VmValue::String(Rc::from(value)))
                .unwrap_or(VmValue::Nil),
            VmValue::Dict(Rc::new(options)),
        ],
    )
    .await
}

pub(crate) async fn call_harn_export_json(
    import_path: &str,
    export_name: &str,
    label: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let result = call_harn_export_by_name(
        import_path,
        export_name,
        label,
        &[crate::stdlib::json_to_vm_value(&payload)],
    )
    .await?;
    Ok(crate::llm::vm_value_to_json(&result))
}

pub(crate) async fn call_harn_export_typed<T>(
    import_path: &str,
    export_name: &str,
    label: &str,
    payload: serde_json::Value,
) -> Result<T, VmError>
where
    T: DeserializeOwned,
{
    let result = call_harn_export_json(import_path, export_name, label, payload).await?;
    serde_json::from_value(result)
        .map_err(|error| VmError::Runtime(format!("{label} returned invalid shape: {error}")))
}
