//! The thread-local interrupt context a blocking builtin observes: the VM's
//! cancellation token, the innermost scope deadline, and the interrupt-handler
//! window, installed around sync builtin dispatch.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Default)]
struct OpInterrupt {
    cancel: Option<Arc<AtomicBool>>,
    /// A scope `deadline` or an operation bound from [`with_deadline`].
    deadline: Option<Instant>,
    /// The `on_interrupt` handler's `graceful_timeout_ms` window. Kept apart
    /// from `deadline` because expiry is a different error kind.
    handler_deadline: Option<Instant>,
    /// An internal setup budget from [`with_operation_budget`]. It stops the
    /// same blocking waits, but its expiry belongs to the operation that set
    /// it, so it is never reported as a caller interrupt.
    operation_budget: Option<Instant>,
}

thread_local! {
    static CURRENT: RefCell<Option<OpInterrupt>> = const { RefCell::new(None) };
}

/// Guard returned by [`install`]. Restores the previously installed
/// interrupt context on drop so nested builtin dispatch (child VMs running
/// on the same thread) composes correctly.
pub struct OpInterruptGuard {
    // Outer Option = "guard owes a restore"; inner Option is the previous
    // thread-local slot value (which can itself be None).
    #[allow(clippy::option_option)]
    prev: Option<Option<OpInterrupt>>,
}

impl Drop for OpInterruptGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            CURRENT.with(|slot| *slot.borrow_mut() = prev);
        }
    }
}

/// Install the interrupt sources a blocking builtin on this thread should
/// observe: an optional cooperative cancel token and an optional deadline.
/// The VM calls this around sync builtin dispatch; tests use it to simulate
/// scope cancellation without booting a full interpreter.
pub fn install(cancel: Option<Arc<AtomicBool>>, deadline: Option<Instant>) -> OpInterruptGuard {
    install_context(OpInterrupt {
        cancel,
        deadline,
        handler_deadline: None,
        operation_budget: None,
    })
}

/// A VM's interrupt sources, captured for blocking work that runs on this
/// thread or is handed to a worker thread. The scope deadline and the
/// interrupt-handler window stay distinct so a VM-less observer reports the
/// same error kind the VM's own interrupt check would.
#[derive(Clone, Default)]
pub struct InterruptSources {
    /// The VM's cooperative cancel token, shared with the owning VM.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The innermost scope `deadline`.
    pub scope_deadline: Option<Instant>,
    /// The `on_interrupt` handler's `graceful_timeout_ms` window.
    pub handler_deadline: Option<Instant>,
}

impl InterruptSources {
    /// The earliest instant at which either deadline stops the operation.
    pub fn earliest_deadline(&self) -> Option<Instant> {
        match (self.scope_deadline, self.handler_deadline) {
            (Some(scope), Some(handler)) => Some(scope.min(handler)),
            (scope, handler) => scope.or(handler),
        }
    }

    /// Whether any source is armed at all.
    pub fn is_armed(&self) -> bool {
        self.cancel.is_some() || self.scope_deadline.is_some() || self.handler_deadline.is_some()
    }

    /// Install these sources on the current thread, keeping each kind apart.
    pub fn install(self) -> OpInterruptGuard {
        install_context(OpInterrupt {
            cancel: self.cancel,
            deadline: self.scope_deadline,
            handler_deadline: self.handler_deadline,
            operation_budget: None,
        })
    }
}

#[cfg(test)]
pub(crate) fn install_for_vm(
    cancel: Option<Arc<AtomicBool>>,
    scope_deadline: Option<Instant>,
    handler_deadline: Option<Instant>,
) -> OpInterruptGuard {
    InterruptSources {
        cancel,
        scope_deadline,
        handler_deadline,
    }
    .install()
}

fn install_context(context: OpInterrupt) -> OpInterruptGuard {
    let prev = CURRENT.with(|slot| slot.borrow_mut().replace(context));
    OpInterruptGuard { prev: Some(prev) }
}

/// Bound a blocking operation, including synchronous process setup, without
/// replacing its caller's cancellation token or extending an earlier deadline.
/// Dropping the guard restores the previous interrupt context.
pub fn with_deadline(deadline: Instant) -> OpInterruptGuard {
    let parent = CURRENT
        .with(|slot| slot.borrow().clone())
        .unwrap_or_default();
    install_context(OpInterrupt {
        deadline: Some(
            parent
                .deadline
                .map_or(deadline, |earlier| earlier.min(deadline)),
        ),
        ..parent
    })
}

/// Bound an internal operation, such as a setup probe, that its caller did not
/// ask to bound. Blocking waits stop when the budget expires, exactly as for
/// [`with_deadline`], but [`requested_error`] does not report the expiry: the
/// operation that installed the budget reads [`operation_budget_expired`] and
/// owns the outcome. Caller cancellation, the handler window, and the scope
/// deadline stay installed and keep their own error kinds.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn with_operation_budget(budget: Instant) -> OpInterruptGuard {
    let parent = CURRENT
        .with(|slot| slot.borrow().clone())
        .unwrap_or_default();
    install_context(OpInterrupt {
        operation_budget: Some(
            parent
                .operation_budget
                .map_or(budget, |earlier| earlier.min(budget)),
        ),
        ..parent
    })
}

/// Returns `true` when an interrupt context is installed on this thread.
///
/// This is separate from [`requested`] so blocking operations can decide
/// whether to use a short heartbeat poll or a true indefinite wait.
pub fn installed() -> bool {
    CURRENT.with(|slot| slot.borrow().is_some())
}

/// Returns `true` when the interrupt context installed on this thread has
/// fired: the cancel token is set, or a deadline or operation budget has
/// passed. Cheap enough
/// to call from a ~20ms poll loop. Returns `false` when nothing is armed.
pub fn requested() -> bool {
    requested_reason().is_some()
}

enum InterruptReason {
    HandlerTimeout,
    Cancelled,
    Deadline,
    OperationBudget,
}

/// Whether only an internal [`with_operation_budget`] stopped the wait. Any
/// caller interrupt takes precedence and reads `false` here.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn operation_budget_expired() -> bool {
    matches!(requested_reason(), Some(InterruptReason::OperationBudget))
}

/// The error a blocking operation with no VM of its own returns once a caller
/// interrupt is requested. Its caller's VM still owns dispatch; this only has
/// to name the same kind that VM's interrupt check would. An expired
/// operation budget is not a caller interrupt and returns `None`.
// Only Linux Bubblewrap preparation consumes it outside tests today.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn requested_error() -> Option<crate::VmError> {
    requested_reason().and_then(|reason| match reason {
        InterruptReason::HandlerTimeout => Some(crate::Vm::interrupt_handler_timeout_error()),
        InterruptReason::Cancelled => Some(crate::cancellation::cancelled_without_machine()),
        InterruptReason::Deadline => Some(crate::Vm::deadline_exceeded_error()),
        InterruptReason::OperationBudget => None,
    })
}

/// Precedence mirrors the VM's interrupt check: an expired handler window,
/// then cancellation, then a scope deadline. An internal operation budget
/// comes last so every caller interrupt keeps its own kind.
fn requested_reason() -> Option<InterruptReason> {
    CURRENT.with(|slot| {
        let ctx = slot.borrow();
        let ctx = ctx.as_ref()?;
        let now = Instant::now();
        if ctx.handler_deadline.is_some_and(|deadline| now >= deadline) {
            return Some(InterruptReason::HandlerTimeout);
        }
        if ctx
            .cancel
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            return Some(InterruptReason::Cancelled);
        }
        if ctx.deadline.is_some_and(|deadline| now >= deadline) {
            return Some(InterruptReason::Deadline);
        }
        if ctx.operation_budget.is_some_and(|budget| now >= budget) {
            return Some(InterruptReason::OperationBudget);
        }
        None
    })
}
