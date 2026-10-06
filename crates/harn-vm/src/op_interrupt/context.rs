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
    })
}

/// The VM's sync-builtin installation: the scope deadline and the
/// interrupt-handler window stay distinct so a VM-less observer reports the
/// same error kind the VM's own interrupt check would.
pub(crate) fn install_for_vm(
    cancel: Option<Arc<AtomicBool>>,
    scope_deadline: Option<Instant>,
    handler_deadline: Option<Instant>,
) -> OpInterruptGuard {
    install_context(OpInterrupt {
        cancel,
        deadline: scope_deadline,
        handler_deadline,
    })
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

/// Returns `true` when an interrupt context is installed on this thread.
///
/// This is separate from [`requested`] so blocking operations can decide
/// whether to use a short heartbeat poll or a true indefinite wait.
pub fn installed() -> bool {
    CURRENT.with(|slot| slot.borrow().is_some())
}

/// Returns `true` when the interrupt context installed on this thread has
/// fired: the cancel token is set, or the deadline has passed. Cheap enough
/// to call from a ~20ms poll loop. Returns `false` when nothing is armed.
pub fn requested() -> bool {
    requested_reason().is_some()
}

enum InterruptReason {
    HandlerTimeout,
    Cancelled,
    Deadline,
}

/// The error a blocking operation with no VM of its own returns once an
/// interrupt is requested. Its caller's VM still owns dispatch; this only has
/// to name the same kind that VM's interrupt check would.
// Only Linux Bubblewrap preparation consumes it outside tests today.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn requested_error() -> Option<crate::VmError> {
    requested_reason().map(|reason| match reason {
        InterruptReason::HandlerTimeout => crate::Vm::interrupt_handler_timeout_error(),
        InterruptReason::Cancelled => crate::cancellation::cancelled_without_machine(),
        InterruptReason::Deadline => crate::Vm::deadline_exceeded_error(),
    })
}

/// Precedence mirrors the VM's interrupt check: an expired handler window,
/// then cancellation, then a scope deadline.
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
        None
    })
}
