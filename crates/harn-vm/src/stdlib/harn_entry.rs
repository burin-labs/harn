//! Registration helpers for public builtins implemented by Harn stdlib modules.

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

#[derive(Clone, Copy, Debug)]
pub(crate) struct HarnAsyncEntrypoint {
    pub public_name: &'static str,
    pub import_path: &'static str,
    pub export_name: &'static str,
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
        }
    }

    fn register(self, vm: &mut Vm) {
        vm.register_async_builtin(self.public_name, move |args| {
            Box::pin(async move { call_harn_export(self, args).await })
        });
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
