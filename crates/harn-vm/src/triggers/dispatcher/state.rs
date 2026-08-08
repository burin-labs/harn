use std::cell::RefCell;
use std::sync::Arc;

use super::types::{DispatchContext, DispatchWaitLease, DispatcherRuntimeState};

thread_local! {
    pub(super) static ACTIVE_DISPATCHER_STATE: RefCell<Option<Arc<DispatcherRuntimeState>>> = const { RefCell::new(None) };
    pub(super) static ACTIVE_DISPATCH_CONTEXT: RefCell<Option<DispatchContext>> = const { RefCell::new(None) };
    pub(super) static ACTIVE_DISPATCH_WAIT_LEASE: RefCell<Option<DispatchWaitLease>> = const { RefCell::new(None) };
    #[cfg(test)]
    pub(super) static TEST_INBOX_DEQUEUED_SIGNAL: RefCell<Option<tokio::sync::oneshot::Sender<()>>> = const { RefCell::new(None) };
}

tokio::task_local! {
    pub(super) static ACTIVE_DISPATCH_IS_REPLAY: bool;
}

pub(crate) fn current_dispatch_context() -> Option<DispatchContext> {
    ACTIVE_DISPATCH_CONTEXT.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_dispatch_is_replay() -> bool {
    ACTIVE_DISPATCH_IS_REPLAY
        .try_with(|is_replay| *is_replay)
        .unwrap_or(false)
}

/// Full replay-detection predicate shared by the stdlib dispatch primitives
/// (monitors and waitpoints). Replay is active when the current dispatch is a
/// replay or the `HARN_REPLAY` environment variable holds a non-empty,
/// non-`"0"` value.
///
/// This is the single owner for the environment read. The ambient
/// `HARN_REPLAY` read is compiled out under `#[cfg(test)]`, so in-process unit
/// tests are hermetic regardless of the invoking shell; replay in tests is
/// driven by the dispatch context. Real binaries still honour `HARN_REPLAY`,
/// which is how a child `harn` process spawned during replay inherits its
/// replay state.
pub(crate) fn is_replay() -> bool {
    current_dispatch_is_replay() || replay_env_flag()
}

#[cfg(not(test))]
fn replay_env_flag() -> bool {
    std::env::var("HARN_REPLAY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty() && value != "0")
}

#[cfg(test)]
fn replay_env_flag() -> bool {
    false
}

pub(crate) fn current_dispatch_wait_lease() -> Option<DispatchWaitLease> {
    ACTIVE_DISPATCH_WAIT_LEASE.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
pub(super) fn install_test_inbox_dequeued_signal(tx: tokio::sync::oneshot::Sender<()>) {
    TEST_INBOX_DEQUEUED_SIGNAL.with(|slot| {
        *slot.borrow_mut() = Some(tx);
    });
}

#[cfg(not(test))]
pub(super) fn notify_test_inbox_dequeued() {}

#[cfg(test)]
pub(super) fn notify_test_inbox_dequeued() {
    TEST_INBOX_DEQUEUED_SIGNAL.with(|slot| {
        if let Some(tx) = slot.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
}
