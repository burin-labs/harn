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

#[cfg(test)]
thread_local! {
    pub(super) static TEST_REPLAY_OVERRIDE: RefCell<Option<bool>> = const { RefCell::new(None) };
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
/// replay, a `#[cfg(test)]` override is installed, or the `HARN_REPLAY`
/// environment variable holds a non-empty, non-`"0"` value.
///
/// This is the single owner for the env read + test seam. The ambient
/// `HARN_REPLAY` read is compiled out under `#[cfg(test)]`, so in-process unit
/// tests are hermetic regardless of the invoking shell — replay in tests is
/// driven only by the dispatch context or the explicit override seam. Real
/// binaries still honour `HARN_REPLAY`, which is how a child `harn` process
/// spawned during replay inherits its replay state.
pub(crate) fn is_replay() -> bool {
    current_dispatch_is_replay() || test_replay_override() || replay_env_flag()
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

#[cfg(test)]
fn test_replay_override() -> bool {
    TEST_REPLAY_OVERRIDE.with(|slot| slot.borrow().unwrap_or(false))
}

#[cfg(not(test))]
fn test_replay_override() -> bool {
    false
}

#[cfg(test)]
pub(crate) struct TestReplayOverrideGuard {
    previous: Option<bool>,
}

#[cfg(test)]
impl Drop for TestReplayOverrideGuard {
    fn drop(&mut self) {
        TEST_REPLAY_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = self.previous;
        });
    }
}

/// Force [`is_replay`] to report the given value on the current thread until the
/// returned guard drops. Test-only seam used to exercise replay code paths
/// without a live dispatch context.
#[cfg(test)]
pub(crate) fn install_test_replay_override(value: bool) -> TestReplayOverrideGuard {
    let previous = TEST_REPLAY_OVERRIDE.with(|slot| slot.borrow_mut().replace(value));
    TestReplayOverrideGuard { previous }
}

#[cfg(test)]
pub(super) fn reset_test_replay_override() {
    TEST_REPLAY_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = None;
    });
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
