//! Cached dispatch for Rust-owned host primitives that delegate policy to Harn stdlib code.

use std::cell::RefCell;

use serde::de::DeserializeOwned;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static HARN_STDLIB_VM_POOL: RefCell<Vec<Vm>> = const { RefCell::new(Vec::new()) };
}

fn take_harn_stdlib_vm() -> Vm {
    HARN_STDLIB_VM_POOL
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| {
            let mut vm = Vm::new();
            crate::stdlib::register_core_stdlib(&mut vm);
            vm
        })
}

fn restore_harn_stdlib_vm(mut vm: Vm) {
    let _ = vm.take_output();
    HARN_STDLIB_VM_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < 8 {
            pool.push(vm);
        }
    });
}

pub(crate) async fn call_harn_stdlib_function(
    module: &str,
    function: &str,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let mut vm = take_harn_stdlib_vm();
    let mut call_vm = vm.child_vm();
    let result = async {
        let exports = call_vm.load_module_exports_from_import(module).await?;
        let closure = exports
            .get(function)
            .cloned()
            .ok_or_else(|| VmError::Runtime(format!("{module} missing {function} export")))?;
        call_vm.call_closure_pub(&closure, args).await
    }
    .await;
    vm.module_cache = call_vm.module_cache.clone();
    vm.source_cache = call_vm.source_cache.clone();
    restore_harn_stdlib_vm(vm);
    result
}

pub(crate) async fn call_harn_stdlib_json(
    module: &str,
    function: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let result = call_harn_stdlib_function(
        module,
        function,
        &[crate::stdlib::json_to_vm_value(&payload)],
    )
    .await?;
    Ok(crate::llm::vm_value_to_json(&result))
}

pub(crate) async fn call_harn_stdlib_typed<T>(
    module: &str,
    function: &str,
    payload: serde_json::Value,
) -> Result<T, VmError>
where
    T: DeserializeOwned,
{
    let result = call_harn_stdlib_json(module, function, payload).await?;
    serde_json::from_value(result)
        .map_err(|error| VmError::Runtime(format!("{function} returned invalid shape: {error}")))
}
