//! Process-wide registry of observers for committed session-metadata changes.
//!
//! File-backed store hooks and WAL polls invalidate the same current-row view.
//! One drain delivers accepted changes in order, outside the state lock.
//! Callbacks can change subscriptions or commit subsequent metadata without
//! taking that lock recursively. In-memory hooks publish their supplied rows
//! directly.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
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
struct RememberedSession {
    database: PathBuf,
    session_id: String,
    meta: Option<SessionMeta>,
    published: bool,
    local_pending: bool,
    retry: bool,
}

enum Notification {
    Metadata(Box<SessionMeta>),
    #[cfg(test)]
    Barrier(std::sync::mpsc::Sender<()>),
}

struct NotificationState {
    sessions: Vec<RememberedSession>,
    pending: VecDeque<Notification>,
    draining: bool,
}

static NOTIFICATIONS: RwLock<NotificationState> = RwLock::new(NotificationState {
    sessions: Vec::new(),
    pending: VecDeque::new(),
    draining: false,
});

fn observers() -> RwLockWriteGuard<'static, Vec<(u64, SharedSessionChangeObserver)>> {
    OBSERVERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn notification_state() -> RwLockWriteGuard<'static, NotificationState> {
    NOTIFICATIONS
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
        {
            let mut observers = observers();
            observers.retain(|(id, _)| *id != self.id);
            if observers.is_empty() {
                let mut state = notification_state();
                state.sessions.clear();
                state.pending.clear();
            }
        }
        session_wal_watch::sync_watchers();
    }
}

/// Register an observer of current session metadata from stores this VM opens
/// and from other processes writing the same file.
///
/// Every canonical store opened here carries the registered observers, so a
/// surface does not have to be handed the specific handle a writer happened to
/// use.
///
/// File-backed notifications project the current committed row. Intermediate
/// rows may coalesce when hooks arrive late or the WAL watcher sees several
/// commits together. An unchanged row is not published again. Accepted rows
/// are delivered in order. Direct in-memory hooks publish their supplied rows
/// without this file-backed coalescing.
pub fn subscribe(observer: SharedSessionChangeObserver) -> SessionChangeSubscription {
    let id = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
    observers().push((id, observer));
    session_wal_watch::sync_watchers();
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
    session_wal_watch::sync_watchers();
    registration
}

pub(super) fn subscriber_count() -> usize {
    OBSERVERS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

#[derive(Clone, Copy)]
enum Observation {
    Baseline,
    Foreign,
    Local,
}

/// A watcher baseline must not replace a snapshot already accepted for delivery.
pub(super) fn seed_session(path: &Path, session_id: &str) {
    invalidate(path, session_id, Observation::Baseline);
}

/// Fans one committed change out to every live subscriber.
pub(super) fn dispatch(path: &Path, meta: &SessionMeta) {
    invalidate(path, &meta.id, Observation::Local);
}

/// A watcher's first encounter establishes its baseline, not an update.
pub(super) fn dispatch_foreign(path: &Path, session_id: &str) {
    invalidate(path, session_id, Observation::Foreign);
}

fn invalidate(path: &Path, session_id: &str, observation: Observation) {
    let mut state = notification_state();
    let index = state
        .sessions
        .iter()
        .position(|seen| seen.database == path && seen.session_id == session_id);
    if matches!(observation, Observation::Baseline)
        && index.is_some_and(|index| state.sessions[index].meta.is_some())
    {
        return;
    }
    let index = index.unwrap_or_else(|| {
        state.sessions.push(RememberedSession {
            database: path.to_owned(),
            session_id: session_id.to_owned(),
            meta: None,
            published: false,
            local_pending: false,
            retry: false,
        });
        state.sessions.len() - 1
    });
    // Keep unresolved hook authority in this same entry. The existing watcher
    // retries it even if SQLite's data_version does not move again.
    state.sessions[index].local_pending |= matches!(observation, Observation::Local);
    state.sessions[index].retry = true;
    // Resolve under the acceptance lock. Neither a delayed store hook nor a
    // WAL snapshot captured before a newer commit can put an older row back.
    let Ok(reader) = harn_session_store::wal_watch::open_watch_reader(path) else {
        return;
    };
    let Ok(meta) = harn_session_store::wal_watch::describe_session(&reader, session_id) else {
        return;
    };
    drop(reader);
    let previous = &mut state.sessions[index];
    let changed = previous.meta.as_ref().is_some_and(|seen| seen != &meta);
    let should_publish = changed || (previous.local_pending && !previous.published);
    previous.meta = Some(meta.clone());
    previous.published |= should_publish;
    previous.local_pending = false;
    previous.retry = false;
    if !should_publish {
        return;
    }
    state
        .pending
        .push_back(Notification::Metadata(Box::new(meta)));
    start_drain(state);
}

pub(super) fn retry_pending(path: &Path) {
    let sessions: Vec<String> = notification_state()
        .sessions
        .iter()
        .filter(|seen| seen.database == path && seen.retry)
        .map(|seen| seen.session_id.clone())
        .collect();
    for session_id in sessions {
        invalidate(path, &session_id, Observation::Foreign);
    }
}

fn start_drain(mut state: RwLockWriteGuard<'static, NotificationState>) {
    if state.draining {
        return;
    }
    state.draining = true;
    drop(state);
    let mut ownership = DrainOwnership(true);
    loop {
        let notification = {
            let mut state = notification_state();
            match state.pending.pop_front() {
                Some(notification) => notification,
                None => {
                    state.draining = false;
                    ownership.0 = false;
                    return;
                }
            }
        };
        match notification {
            Notification::Metadata(meta) => publish(&meta),
            #[cfg(test)]
            Notification::Barrier(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

/// A panicking observer must not leave future changes without a drain owner.
struct DrainOwnership(bool);

impl Drop for DrainOwnership {
    fn drop(&mut self) {
        if self.0 {
            notification_state().draining = false;
        }
    }
}

#[cfg(test)]
pub(super) fn acknowledge_delivery(ack: std::sync::mpsc::Sender<()>) {
    let mut state = notification_state();
    state.pending.push_back(Notification::Barrier(ack));
    start_drain(state);
}

fn publish(meta: &SessionMeta) {
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

struct SessionChangeFanout {
    database: Option<PathBuf>,
}

impl harn_session_store::SessionChangeObserver for SessionChangeFanout {
    fn session_updated(&self, meta: &SessionMeta) {
        if let Some(path) = self.database.as_deref() {
            dispatch(path, meta);
        } else {
            // In-memory imports have no file watcher and no shared database
            // identity, so they do not participate in file deduplication.
            publish(meta);
        }
    }
}

pub(crate) fn current_observer(database: Option<&Path>) -> Option<SharedSessionChangeObserver> {
    if subscriber_count() == 0 {
        return None;
    }
    Some(Arc::new(SessionChangeFanout {
        database: database.map(Path::to_owned),
    }))
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use harn_clock::test_support::recv_within;
    use harn_session_store::{
        CreateSession, SessionStore, SqliteSessionStore, StoreHooks, UpdateSession,
    };

    use super::*;

    async fn observed_store(
        observer: SharedSessionChangeObserver,
    ) -> (
        tempfile::TempDir,
        SqliteSessionStore,
        SessionChangeSubscription,
        SessionMeta,
    ) {
        let root = tempfile::tempdir().expect("workspace");
        let canonical = crate::open_canonical_store(root.path()).expect("canonical store");
        let before = canonical
            .create(CreateSession {
                id: Some("ordered".into()),
                ..Default::default()
            })
            .await
            .expect("create session");
        let path = canonical.path().to_owned();
        drop(canonical);
        let subscription = subscribe(observer);
        let writer = SqliteSessionStore::open_with_hooks(
            &path,
            StoreHooks {
                change_observer: current_observer(Some(&path)),
                ..Default::default()
            },
        )
        .expect("writer using the canonical change fanout");
        (root, writer, subscription, before)
    }

    struct Recording(mpsc::Sender<Option<String>>);

    impl harn_session_store::SessionChangeObserver for Recording {
        fn session_updated(&self, meta: &SessionMeta) {
            self.0
                .send(meta.title.clone())
                .expect("notification receiver");
        }
    }

    #[tokio::test]
    async fn a_delayed_foreign_snapshot_cannot_replace_a_committed_title() {
        let _bus = test_support::exclusive_bus().await;
        let (tx, rx) = mpsc::channel();
        let (_root, writer, _subscription, before) = observed_store(Arc::new(Recording(tx))).await;
        assert_eq!(before.title, None, "the saved snapshot predates the rename");
        writer
            .update(
                "ordered",
                UpdateSession {
                    title: Some("committed title".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("commit title");
        assert_eq!(
            recv_within("committed title", &rx).as_deref(),
            Some("committed title")
        );

        // A WAL reader can finish delivering a row read before the local hook.
        dispatch_foreign(writer.path(), &before.id);
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "the delayed snapshot must not revert the client to a titleless row"
        );
    }

    #[tokio::test]
    async fn a_delayed_local_hook_coalesces_after_a_newer_title_is_delivered() {
        let _bus = test_support::exclusive_bus().await;
        let (tx, rx) = mpsc::channel();
        let (_root, writer, _subscription, _) = observed_store(Arc::new(Recording(tx))).await;
        let (entered, paused) = mpsc::channel();
        let (resume, barrier) = mpsc::channel();
        struct DelayedHook {
            entered: mpsc::Sender<()>,
            resume: std::sync::Mutex<mpsc::Receiver<()>>,
            fanout: SharedSessionChangeObserver,
        }
        impl harn_session_store::SessionChangeObserver for DelayedHook {
            fn session_updated(&self, meta: &SessionMeta) {
                self.entered
                    .send(())
                    .expect("first commit reached its hook");
                recv_within("release delayed local hook", &self.resume.lock().unwrap());
                self.fanout.session_updated(meta);
            }
        }
        let delayed = SqliteSessionStore::open_with_hooks(
            writer.path(),
            StoreHooks {
                change_observer: Some(Arc::new(DelayedHook {
                    entered,
                    resume: std::sync::Mutex::new(barrier),
                    fanout: writer.hooks().change_observer.as_ref().unwrap().clone(),
                })),
                ..Default::default()
            },
        )
        .expect("writer with a delayed post-commit hook");
        let runtime = tokio::runtime::Handle::current();
        let first = crate::runtime_stack::spawn(move || {
            runtime.block_on(delayed.update(
                "ordered",
                UpdateSession {
                    title: Some("first".into()),
                    ..Default::default()
                },
            ))
        });
        recv_within("first committed before the newer update", &paused);
        writer
            .update(
                "ordered",
                UpdateSession {
                    title: Some("second".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("newer commit");
        assert_eq!(
            recv_within("newer title reached the subscriber", &rx).as_deref(),
            Some("second")
        );
        resume.send(()).expect("release the earlier hook");
        let first_row = first.join().expect("first writer").expect("first commit");
        assert_eq!(first_row.title.as_deref(), Some("first"));
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "the late hook must publish neither the stale row nor a duplicate newer row"
        );
    }

    struct PausedFirst {
        entered: mpsc::Sender<()>,
        resume: std::sync::Mutex<mpsc::Receiver<()>>,
        delivered: mpsc::Sender<Option<String>>,
    }

    impl harn_session_store::SessionChangeObserver for PausedFirst {
        fn session_updated(&self, meta: &SessionMeta) {
            if meta.title.as_deref() == Some("first") {
                self.entered.send(()).expect("publication barrier");
                recv_within("resume first publication", &self.resume.lock().unwrap());
            }
            self.delivered
                .send(meta.title.clone())
                .expect("notification receiver");
        }
    }

    #[tokio::test]
    async fn committed_notifications_are_delivered_in_acceptance_order() {
        let _bus = test_support::exclusive_bus().await;
        let (entered, paused) = mpsc::channel();
        let (resume, barrier) = mpsc::channel();
        let (delivered, rx) = mpsc::channel();
        let (_root, writer, _subscription, _) = observed_store(Arc::new(PausedFirst {
            entered,
            resume: std::sync::Mutex::new(barrier),
            delivered,
        }))
        .await;
        let first_writer = writer.clone();
        let runtime = tokio::runtime::Handle::current();
        let first = crate::runtime_stack::spawn(move || {
            runtime.block_on(first_writer.update(
                "ordered",
                UpdateSession {
                    title: Some("first".into()),
                    ..Default::default()
                },
            ))
        });
        recv_within("first notification reached the observer", &paused);
        writer
            .update(
                "ordered",
                UpdateSession {
                    title: Some("second".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("second commit");
        resume.send(()).expect("release first notification");
        first
            .join()
            .expect("first writer thread")
            .expect("first commit");

        assert_eq!(
            recv_within("first delivered title", &rx).as_deref(),
            Some("first")
        );
        assert_eq!(
            recv_within("second delivered title", &rx).as_deref(),
            Some("second")
        );
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn a_watch_baseline_cannot_swallow_the_local_commit_notification() {
        let _bus = test_support::exclusive_bus().await;
        let (tx, rx) = mpsc::channel();
        let (_root, writer, _subscription, _) = observed_store(Arc::new(Recording(tx))).await;
        let fanout = writer.hooks().change_observer.as_ref().unwrap();
        // Commit without the hook, as if a watcher establishes its baseline
        // in the interval before the real writer invokes that hook.
        let delayed = SqliteSessionStore::open(writer.path()).unwrap();
        let committed = delayed
            .update(
                "ordered",
                UpdateSession {
                    title: Some("committed before baseline".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        seed_session(writer.path(), "ordered");
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        fanout.session_updated(&committed);
        assert_eq!(
            recv_within("local commit after baseline", &rx).as_deref(),
            Some("committed before baseline")
        );
        dispatch_foreign(writer.path(), "ordered");
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    struct ReplacingObserver {
        original: std::sync::Mutex<Option<SessionChangeSubscription>>,
        replacement: std::sync::Mutex<Option<SessionChangeSubscription>>,
        original_delivered: mpsc::Sender<()>,
        replacement_delivered: mpsc::Sender<Option<String>>,
    }

    impl harn_session_store::SessionChangeObserver for ReplacingObserver {
        fn session_updated(&self, _meta: &SessionMeta) {
            *self.replacement.lock().unwrap() = Some(subscribe(Arc::new(Recording(
                self.replacement_delivered.clone(),
            ))));
            drop(self.original.lock().unwrap().take());
            self.original_delivered.send(()).unwrap();
        }
    }

    #[tokio::test]
    async fn an_observer_can_replace_its_subscription_during_delivery() {
        let _bus = test_support::exclusive_bus().await;
        let (original_delivered, original_rx) = mpsc::channel();
        let (replacement_delivered, replacement_rx) = mpsc::channel();
        let observer = Arc::new(ReplacingObserver {
            original: std::sync::Mutex::new(None),
            replacement: std::sync::Mutex::new(None),
            original_delivered,
            replacement_delivered,
        });
        let (_root, writer, subscription, _) = observed_store(observer.clone()).await;
        *observer.original.lock().unwrap() = Some(subscription);
        for title in ["first", "second"] {
            writer
                .update(
                    "ordered",
                    UpdateSession {
                        title: Some(title.into()),
                        ..Default::default()
                    },
                )
                .await
                .expect("title commit");
            if title == "first" {
                recv_within("observer replaced its subscription", &original_rx);
                assert_eq!(replacement_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
            }
        }
        assert_eq!(
            recv_within(
                "replacement observer receives the next commit",
                &replacement_rx
            )
            .as_deref(),
            Some("second")
        );
        assert_eq!(replacement_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn an_observer_can_commit_another_metadata_update_during_delivery() {
        let _bus = test_support::exclusive_bus().await;
        struct RenameAgain {
            writer: std::sync::Mutex<Option<SqliteSessionStore>>,
            delivered: mpsc::Sender<Option<String>>,
        }
        impl harn_session_store::SessionChangeObserver for RenameAgain {
            fn session_updated(&self, meta: &SessionMeta) {
                self.delivered.send(meta.title.clone()).unwrap();
                if meta.title.as_deref() == Some("first") {
                    let writer = self.writer.lock().unwrap().as_ref().unwrap().clone();
                    futures::executor::block_on(writer.update(
                        "ordered",
                        UpdateSession {
                            title: Some("second".into()),
                            ..Default::default()
                        },
                    ))
                    .expect("callback commits the next metadata update");
                }
            }
        }
        let (delivered, rx) = mpsc::channel();
        let observer = Arc::new(RenameAgain {
            writer: std::sync::Mutex::new(None),
            delivered,
        });
        let (_root, writer, _subscription, _) = observed_store(observer.clone()).await;
        *observer.writer.lock().unwrap() = Some(writer.clone());
        writer
            .update(
                "ordered",
                UpdateSession {
                    title: Some("first".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            recv_within("first callback update", &rx).as_deref(),
            Some("first")
        );
        assert_eq!(
            recv_within("reentrant callback update", &rx).as_deref(),
            Some("second")
        );
        assert_eq!(
            writer.describe("ordered").await.unwrap().title.as_deref(),
            Some("second")
        );
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }
}
