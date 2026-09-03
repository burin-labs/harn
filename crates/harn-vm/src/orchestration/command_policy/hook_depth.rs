//! How deep the VM is inside its own command-policy hooks.
//!
//! A `command_policy` may install a `pre` hook and a `consent` gate. Both are
//! Harn's policy machinery, not the model's work, and two decisions depend on
//! being able to tell the difference: recursion control refuses a hook that
//! re-enters the command runner, and capability enforcement lets the consent
//! gate ask its host without the calling tool having to declare a `permission`
//! capability it has no other use for.
//!
//! The depth lives here rather than beside the policy stack so that
//! distinction has one owner. `AmbientExecutionScope` swaps it per task, which
//! is what makes it correct across an `.await`: a guard held over one would
//! otherwise be visible to a sibling task interleaving on the same thread.

use std::cell::RefCell;

thread_local! {
    static COMMAND_POLICY_HOOK_DEPTH: RefCell<usize> = const { RefCell::new(0) };
}

/// Enter one of the VM's own command-policy hooks. The returned guard leaves
/// the hook on drop.
pub(crate) fn enter_command_policy_hook() -> HookDepthGuard {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow_mut() += 1);
    HookDepthGuard
}

pub(crate) struct HookDepthGuard;

impl Drop for HookDepthGuard {
    fn drop(&mut self) {
        COMMAND_POLICY_HOOK_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

/// Whether the VM is currently inside one of its own command-policy hooks.
pub fn command_policy_hook_depth() -> usize {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow())
}

pub(crate) fn reset_command_policy_hook_depth() {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

/// Per-task ambient-scope swap. See `orchestration::ambient_scope`.
pub(crate) fn swap_command_policy_hook_depth(next: usize) -> usize {
    COMMAND_POLICY_HOOK_DEPTH.with(|depth| std::mem::replace(&mut *depth.borrow_mut(), next))
}
