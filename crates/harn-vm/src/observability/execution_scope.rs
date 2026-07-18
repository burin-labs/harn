//! Ambient EXECUTION SCOPE — an immutable owner token minted once per Harn
//! program run and readable, like [`crate::observability::request_id`], from
//! hostlib builtins on the synchronous dispatch stack.
//!
//! Unlike `request_id` (which a *host* pushes only at served ingress and which
//! is `None` for standalone `harn run`/`harn test`), the execution scope is
//! established by the VM itself at the top-level program boundary
//! (`Vm::execute_scoped`), so it is ALWAYS present during real execution and is
//! never a shared sentinel. That is exactly what lets the verdict issuance
//! authority bind a proof-of-execution receipt to the specific run that
//! PRODUCED the evidence: `run_test` captures the active scope when it records a
//! real execution, and `harness.verdict.issue` mints a positive verdict only
//! when the current active scope EQUALS that captured owner — so an old green
//! handle cannot bless a later, different run (the cross-run replay class).
//!
//! The scope is a stack so nested program executions restore the outer owner on
//! return; the innermost entry wins for [`current_execution_scope`]. When no
//! scope is active (there is no owning execution), issuance FAILS CLOSED — it
//! never falls back to a default owner.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

thread_local! {
    static ACTIVE_EXECUTION_SCOPE_STACK: RefCell<Vec<Arc<str>>> = const { RefCell::new(Vec::new()) };
}

static SCOPE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Mint a fresh, process-unique, immutable execution-scope id. The `pid`
/// prefix keeps ids from colliding across processes that share the (in-process)
/// result store's namespace; the monotonic counter keeps them distinct within
/// one process. Callers push the returned id via [`enter_execution_scope`].
pub fn mint_execution_scope() -> Arc<str> {
    let n = SCOPE_COUNTER.fetch_add(1, Ordering::SeqCst);
    Arc::from(format!("hxs-{:x}-{n}", std::process::id()).as_str())
}

/// RAII guard returned by [`enter_execution_scope`]. Popping the stack on drop
/// keeps the ambient balanced even when the enclosed program run panics or
/// returns an error.
#[must_use = "dropping the guard immediately pops the execution scope"]
pub struct ExecutionScopeGuard {
    _private: (),
}

impl Drop for ExecutionScopeGuard {
    fn drop(&mut self) {
        ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `scope` onto the ambient stack for the lifetime of the returned guard.
/// The innermost entry wins for [`current_execution_scope`].
pub fn enter_execution_scope(scope: Arc<str>) -> ExecutionScopeGuard {
    ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| stack.borrow_mut().push(scope));
    ExecutionScopeGuard { _private: () }
}

/// Currently-active execution scope, or `None` when no owning program run is
/// active on this task. Verdict issuance treats `None` as fail-closed.
pub fn current_execution_scope() -> Option<Arc<str>> {
    ACTIVE_EXECUTION_SCOPE_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Replace the whole ambient stack, returning the previous one. Used by the
/// orchestration ambient-scope machinery to carry the owner across
/// `spawn_local` fan-out boundaries (which plain thread-locals do not cross),
/// mirroring how the current-session stack is propagated.
pub(crate) fn swap_execution_scope_stack(replacement: Vec<Arc<str>>) -> Vec<Arc<str>> {
    ACTIVE_EXECUTION_SCOPE_STACK
        .with(|stack| std::mem::replace(&mut *stack.borrow_mut(), replacement))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_none_when_nothing_pushed() {
        // A fresh thread has no owning execution.
        std::thread::spawn(|| {
            assert_eq!(current_execution_scope(), None);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn guard_pops_on_drop_and_inner_shadows_outer() {
        std::thread::spawn(|| {
            let outer = mint_execution_scope();
            let inner = mint_execution_scope();
            assert_ne!(outer, inner);
            let _o = enter_execution_scope(outer.clone());
            assert_eq!(current_execution_scope().as_deref(), Some(&*outer));
            {
                let _i = enter_execution_scope(inner.clone());
                assert_eq!(current_execution_scope().as_deref(), Some(&*inner));
            }
            assert_eq!(current_execution_scope().as_deref(), Some(&*outer));
        })
        .join()
        .unwrap();
    }
}
