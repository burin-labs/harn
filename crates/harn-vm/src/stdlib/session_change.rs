//! Process-wide registry of observers for committed session-metadata changes.
//!
//! Split out of `session_store` rather than living beside the store opener: it
//! is a notification concern, not a storage one, and the two have no shared
//! state beyond the hook the opener attaches.
//!
//! Writes in this process still publish immediately through the store hook.
//! Writes from another process reach the same SQLite file; [`super::session_wal_watch`]
//! notices those via WAL + `PRAGMA data_version` and publishes through the
//! same fanout.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard};

use harn_session_store::{SessionMeta, SharedSessionChangeObserver};

use super::session_wal_watch;

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
type TitleFingerprint = (Option<String>, bool);
type RememberedTitles = Vec<(String, TitleFingerprint)>;
static TITLES: RwLock<RememberedTitles> = RwLock::new(Vec::new());

fn observers() -> RwLockWriteGuard<'static, Vec<(u64, SharedSessionChangeObserver)>> {
    OBSERVERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn titles() -> RwLockWriteGuard<'static, RememberedTitles> {
    TITLES
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
        let live = subscriber_count() > 0;
        if !live {
            titles().clear();
        }
        session_wal_watch::sync_watchers(live);
    }
}

/// Register an observer notified after any session metadata update commits
/// through a store this VM opens, or after a WAL watcher sees another
/// process rename a session in the same file.
///
/// Every canonical store opened here carries the registered observers, so a
/// surface does not have to be handed the specific handle a writer happened to
/// use.
pub fn subscribe(observer: SharedSessionChangeObserver) -> SessionChangeSubscription {
    let id = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
    observers().push((id, observer));
    session_wal_watch::sync_watchers(true);
    SessionChangeSubscription { id }
}

/// Claim a canonical store file so a live subscription can watch it.
///
/// The returned registration belongs to the store handle that opened the file.
/// While it lives the path is watchable; when the last handle for that path
/// drops, so does the watcher.
#[must_use = "dropping the registration stops the path being watched"]
pub(crate) fn watch_store(path: &Path) -> session_wal_watch::StoreWatchRegistration {
    let registration = session_wal_watch::register_store_path(path);
    if subscriber_count() > 0 {
        session_wal_watch::sync_watchers(true);
    }
    registration
}

fn subscriber_count() -> usize {
    OBSERVERS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

/// Whether a title/pin pair is new to this process, unchanged, or moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TitleMemory {
    New,
    Unchanged,
    Changed,
}

pub(super) fn remember_title(
    session_id: &str,
    title: Option<&str>,
    title_pinned: bool,
) -> TitleMemory {
    let next = (title.map(str::to_string), title_pinned);
    let mut titles = titles();
    if let Some((_, previous)) = titles.iter_mut().find(|(id, _)| id == session_id) {
        if *previous == next {
            return TitleMemory::Unchanged;
        }
        *previous = next;
        return TitleMemory::Changed;
    }
    titles.push((session_id.to_string(), next));
    TitleMemory::New
}

/// Fans one committed change out to every live subscriber.
pub(super) fn dispatch(meta: &SessionMeta) {
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

struct SessionChangeFanout;

impl harn_session_store::SessionChangeObserver for SessionChangeFanout {
    fn session_updated(&self, meta: &SessionMeta) {
        remember_title(&meta.id, meta.title.as_deref(), meta.title_pinned);
        dispatch(meta);
    }
}

pub(crate) fn current_observer() -> Option<SharedSessionChangeObserver> {
    if subscriber_count() == 0 {
        return None;
    }
    Some(Arc::new(SessionChangeFanout))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::OnceLock;

    use tokio::sync::{Mutex, MutexGuard};

    /// Exclusive access to the process-wide change bus for one test.
    ///
    /// The observer list, the remembered titles and the running watchers are
    /// one process singleton, the way the process environment is. Two cases
    /// that subscribe at the same time each receive the other's committed
    /// titles, because a subscriber is registered against the process and not
    /// against a store: the double-publish case then read a sibling's rename
    /// as its own republish (harn#7960). One lock, held for the life of the
    /// subscription, is what makes each case see only its own traffic.
    ///
    /// An async mutex rather than a `std` one because every case that needs it
    /// holds it across an await, which is exactly what a blocking guard must
    /// not do. It also has no poisoning to recover: the lock guards no
    /// invariant of its own, so a panicking holder leaves nothing behind.
    #[must_use = "the bus is shared for as long as the guard lives"]
    pub(crate) async fn exclusive_bus() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
    }
}
