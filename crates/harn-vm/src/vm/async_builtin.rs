use std::cell::RefCell;

use super::Vm;

thread_local! {
    pub(super) static CURRENT_ASYNC_BUILTIN_CHILD_VM: RefCell<Vec<Vm>> =
        const { RefCell::new(Vec::new()) };
}

/// Clone the VM at the top of the async-builtin child VM stack, returning a
/// fresh `Vm` instance the caller owns. Enables concurrent tool-handler
/// execution within a single agent_loop iteration — the VM shares its heavy
/// state (env, builtins, bridge, module_cache) via `Arc`/`Rc`, so cloning is
/// cheap and each handler gets its own execution context.
///
/// Returns `None` if no parent VM is currently pushed on the stack.
pub fn clone_async_builtin_child_vm() -> Option<Vm> {
    CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| slot.borrow().last().map(|vm| vm.child_vm()))
}

pub struct AsyncBuiltinChildVmGuard;

pub fn install_async_builtin_child_vm(vm: Vm) -> AsyncBuiltinChildVmGuard {
    CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
        slot.borrow_mut().push(vm);
    });
    AsyncBuiltinChildVmGuard
}

impl Drop for AsyncBuiltinChildVmGuard {
    fn drop(&mut self) {
        CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
            slot.borrow_mut().pop();
        });
    }
}

/// Legacy API preserved for out-of-tree callers; new code should use
/// `clone_async_builtin_child_vm()`. `take/restore` serialized concurrent
/// callers because only one could hold the popped value at a time.
#[deprecated(
    note = "use clone_async_builtin_child_vm() — take/restore serialized concurrent callers"
)]
pub fn take_async_builtin_child_vm() -> Option<Vm> {
    clone_async_builtin_child_vm()
}

/// Legacy no-op retained for backward compatibility.
#[deprecated(note = "clone_async_builtin_child_vm does not need a matching restore call")]
pub fn restore_async_builtin_child_vm(_vm: Vm) {
    CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
        let _ = slot;
    });
}
