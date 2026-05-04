use crate::value::{VmError, VmValue};

pub(crate) async fn call_workflow_stdlib_function(
    module: &str,
    function: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let mut vm = crate::vm::Vm::new();
    crate::stdlib::register_core_stdlib(&mut vm);
    let exports = vm.load_module_exports_from_import(module).await?;
    let closure = exports
        .get(function)
        .cloned()
        .ok_or_else(|| VmError::Runtime(format!("{module} missing {function} export")))?;
    vm.call_closure_pub(&closure, args).await
}
