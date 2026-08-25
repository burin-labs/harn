//! Per-execution security policy state.

use super::{SecurityPolicy, SECURITY_POLICY_STACK};

/// The installed policy, or the spotlight-on default when the stack is empty.
pub fn current_policy() -> SecurityPolicy {
    SECURITY_POLICY_STACK.with(|stack| stack.borrow().last().cloned().unwrap_or_default())
}

/// Swap the policy stack for `AmbientExecutionScope`'s per-poll restoration.
/// An uncaptured empty stack would silently downgrade a strict subtask.
pub(crate) fn swap_security_policy_stack(next: Vec<SecurityPolicy>) -> Vec<SecurityPolicy> {
    SECURITY_POLICY_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}
