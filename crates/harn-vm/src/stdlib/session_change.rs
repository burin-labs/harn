//! Process-wide registry of observers for committed session-metadata changes.
//!
//! Split out of `session_store` rather than living beside the store opener: it
//! is a notification concern, not a storage one, and the two have no shared
//! state beyond the hook the opener attaches.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard};

use harn_session_store::SharedSessionChangeObserver;

/// Process-wide observers notified when session metadata is committed.
///
/// Deliberately process-scoped, not thread-local like the redaction policy
/// beside it: a store handle is opened on whatever thread needs it, and an
/// update commits on whatever executor thread the caller happens to be on. A
/// thread-local sink would be installed on one thread and silently miss every
/// write made from another, which reads as "notifications do not work" rather
/// than as a wiring mistake.
///
/// A list rather than one slot, because more than one surface can care about
/// the same store and a single slot makes the second registration silently
/// evict the first.
static OBSERVERS: RwLock<Vec<(u64, SharedSessionChangeObserver)>> = RwLock::new(Vec::new());
static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(1);

fn observers() -> RwLockWriteGuard<'static, Vec<(u64, SharedSessionChangeObserver)>> {
    OBSERVERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Live registration for [`subscribe`]. Unregisters on drop so
/// a surface that goes away cannot keep receiving, and so a test cannot leak a
/// sink into the next one sharing the process.
#[must_use = "dropping the subscription immediately unregisters the observer"]
pub struct SessionChangeSubscription {
    id: u64,
}

impl Drop for SessionChangeSubscription {
    fn drop(&mut self) {
        observers().retain(|(id, _)| *id != self.id);
    }
}

/// Register an observer notified after any session metadata update commits
/// through a store this VM opens.
///
/// Every canonical store opened here carries the registered observers, so a
/// surface does not have to be handed the specific handle a writer happened to
/// use. Scope is this process only: a write from another process reaches the
/// same database file but no in-process sink.
pub fn subscribe(observer: SharedSessionChangeObserver) -> SessionChangeSubscription {
    let id = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
    observers().push((id, observer));
    SessionChangeSubscription { id }
}

/// Fans one committed change out to every live subscriber.
struct SessionChangeFanout;

impl harn_session_store::SessionChangeObserver for SessionChangeFanout {
    fn session_updated(&self, meta: &harn_session_store::SessionMeta) {
        let observers: Vec<SharedSessionChangeObserver> = OBSERVERS
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(_, observer)| Arc::clone(observer))
            .collect();
        // Copy out before dispatching: an observer is allowed to subscribe or
        // unsubscribe in response, which would deadlock against a held guard.
        for observer in observers {
            observer.session_updated(meta);
        }
    }
}

pub(crate) fn current_observer() -> Option<SharedSessionChangeObserver> {
    let empty = OBSERVERS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty();
    if empty {
        return None;
    }
    Some(Arc::new(SessionChangeFanout))
}
