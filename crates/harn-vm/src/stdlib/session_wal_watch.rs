//! Cross-process session-metadata watch via WAL + `PRAGMA data_version`.
//!
//! One active thread per database path. Retiring threads remain owned until
//! their readers close. Each thread keeps its own reader so it never
//! takes the store mutex, and it only publishes when a metadata snapshot
//! moved. Local writes go through [`super::session_change`], whose current-row
//! view suppresses unchanged invalidations from either source.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use harn_session_store::wal_watch::{
    data_version, list_title_snapshots, open_watch_reader, wal_sidecar_path,
};
use notify::{RecursiveMode, Watcher};

const POLL: Duration = Duration::from_millis(250);

/// What wakes the watch loop out of its poll wait. The loop never inspects a
/// filesystem event: any wake means "read `data_version` now".
enum WatchSignal {
    /// Filesystem activity near the database, or a stop request.
    Wake,
    /// Test barrier: poll once, then acknowledge. The acknowledgement proves
    /// the loop read `data_version` after every commit made before the
    /// request, and published whatever that read found.
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

impl WatchSignal {
    /// Called after the poll that followed this signal.
    fn polled(self) {
        match self {
            Self::Wake => {}
            #[cfg(test)]
            Self::Flush(ack) => {
                super::session_change::acknowledge_delivery(ack);
            }
        }
    }
}

struct WatchState {
    stop: Arc<AtomicBool>,
    /// Wakes the loop out of its poll wait so a stop is observed at once.
    /// Without it, joining costs up to one whole [`POLL`] interval, and a
    /// store handle closing on a hot path would pay that every time.
    wake: mpsc::Sender<WatchSignal>,
    thread_id: std::thread::ThreadId,
    thread: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    joining: Mutex<Option<mpsc::Sender<()>>>,
}

/// Declared outside watch_loop so its reader is already closed on normal
/// return or unwinding before the registry releases this thread's ownership.
struct WatchExit {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for WatchExit {
    fn drop(&mut self) {
        watchers()
            .retain(|(seen, state)| seen != &self.path || !Arc::ptr_eq(&state.stop, &self.stop));
    }
}

/// Canonical store paths with at least one open handle in this process, each
/// with the number of handles holding it.
///
/// Counted rather than a plain set, because "which stores may be watched" is a
/// lifetime question and a set has no answer to it. A path that is only ever
/// added accumulates: this process then attaches a reader, a filesystem
/// watcher and a thread to every database it has ever opened, including ones
/// whose handle is long gone. In the test binary that is also how one case
/// reaches into another's temporary directory — a subscription taken by one
/// case starts a watcher over a sibling's store and leaves `-wal` and `-shm`
/// sidecars in a directory the sibling is asserting on (harn#7960).
static REGISTERED: RwLock<Vec<(PathBuf, usize)>> = RwLock::new(Vec::new());
static WATCHERS: RwLock<Vec<(PathBuf, Arc<WatchState>)>> = RwLock::new(Vec::new());

fn registered() -> std::sync::RwLockWriteGuard<'static, Vec<(PathBuf, usize)>> {
    REGISTERED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn watchers() -> std::sync::RwLockWriteGuard<'static, Vec<(PathBuf, Arc<WatchState>)>> {
    WATCHERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One open handle's claim on watching a canonical store path.
///
/// Held by the store handle itself, so the path stays watchable for exactly as
/// long as something in this process has the store open. A last external drop
/// waits for orphaned readers to close. A drop on the watcher callback itself
/// requests shutdown; its reader closes when that callback returns.
#[must_use = "dropping the registration stops the path being watched"]
pub(crate) struct StoreWatchRegistration {
    /// `None` for a path that is not watchable at all, such as `:memory:`.
    path: Option<PathBuf>,
}

impl Drop for StoreWatchRegistration {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        {
            let mut paths = registered();
            if let Some(index) = paths.iter().position(|(seen, _)| seen == &path) {
                paths[index].1 = paths[index].1.saturating_sub(1);
                if paths[index].1 == 0 {
                    paths.remove(index);
                }
            }
        }
        sync_watchers();
    }
}

/// Claim a canonical store path so a subscription may watch it.
pub(super) fn register_store_path(path: &Path) -> StoreWatchRegistration {
    if path == Path::new(":memory:") {
        return StoreWatchRegistration { path: None };
    }
    let path = path.to_path_buf();
    {
        let mut paths = registered();
        match paths.iter_mut().find(|(seen, _)| seen == &path) {
            Some(entry) => entry.1 += 1,
            None => paths.push((path.clone(), 1)),
        }
    }
    StoreWatchRegistration { path: Some(path) }
}

/// Reconcile readers against the current store claims and subscriptions.
pub(super) fn sync_watchers() {
    // Sample and act under the same registry guard. Subscription changes
    // release OBSERVERS before reconciling, so there is no inverse lock order.
    let mut running = watchers();
    let subscribers_live = super::session_change::subscriber_count() > 0;
    let paths: Vec<PathBuf> = registered().iter().map(|(path, _)| path.clone()).collect();
    let stopping = running
        .iter()
        .filter(|(path, _)| !subscribers_live || !paths.contains(path))
        .map(|(_, state)| {
            state.stop.store(true, Ordering::Relaxed);
            Arc::clone(state)
        })
        .collect::<Vec<_>>();
    if subscribers_live {
        for path in paths {
            if !path.is_file() {
                continue;
            }
            start_watcher(path, &mut running);
        }
    }
    drop(running);
    for state in stopping {
        stop_and_join(state);
    }
}

/// Signal one watcher and wait for its reader to close.
///
/// The wake is a send rather than a channel drop: the loop's own filesystem
/// callback holds the other sender, so closing this one would not disconnect
/// the receiver and the join would still wait out a poll interval.
fn stop_and_join(state: Arc<WatchState>) {
    let _ = state.wake.send(WatchSignal::Wake);
    // A callback cannot wait for itself. Keep its entry and handle owned until
    // the loop returns, so an external store close can still wait for it.
    if state.thread_id == std::thread::current().id() {
        return;
    }
    let mut thread = state
        .thread
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(handle) = thread.take() {
        #[cfg(test)]
        if let Some(joining) = state.joining.lock().unwrap().take() {
            let _ = joining.send(());
        }
        // Concurrent stop callers wait on this mutex until the join completes.
        // The exiting thread only needs WATCHERS, which is not held here.
        let _ = handle.join();
    }
}

/// How many active or retiring readers are owned for `path`. Test-only: assert
/// their lifetime directly rather than inferring it from SQLite sidecars.
#[cfg(test)]
pub(super) fn watcher_count_for(path: &Path) -> usize {
    watchers().iter().filter(|(seen, _)| seen == path).count()
}

/// Block until the watcher for `path` has polled after every commit made
/// before this call, and published what that poll found. Test-only: it is
/// the positive barrier that lets a case assert a publish did *not* happen
/// without waiting out a guessed quiet period.
#[cfg(test)]
pub(super) fn flush_watcher(path: &Path) {
    let wake = watchers()
        .iter()
        .find(|(seen, state)| seen == path && !state.stop.load(Ordering::Relaxed))
        .map(|(_, state)| state.wake.clone())
        .expect("a watcher is running for this path");
    let (ack, acked) = mpsc::channel();
    wake.send(WatchSignal::Flush(ack))
        .expect("the watch loop is running");
    harn_clock::test_support::recv_within("session WAL watcher flush", &acked);
}

/// Record every current metadata snapshot as already seen, so the first poll
/// publishes only what moves after this point.
fn seed_session_memory(path: &Path, reader: &rusqlite::Connection) {
    if let Ok(snapshots) = list_title_snapshots(reader) {
        for snapshot in snapshots {
            super::session_change::seed_session(path, &snapshot.id);
        }
    }
}

fn start_watcher(path: PathBuf, running: &mut Vec<(PathBuf, Arc<WatchState>)>) {
    if running
        .iter()
        .any(|(seen, state)| seen == &path && !state.stop.load(Ordering::Relaxed))
    {
        return;
    }
    let Ok(reader) = open_watch_reader(&path) else {
        return;
    };
    let Ok(initial_version) = data_version(&reader) else {
        return;
    };
    seed_session_memory(&path, &reader);

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let cleanup_stop = Arc::clone(&stop);
    let thread_path = path.clone();
    let (wake, events) = mpsc::channel();
    let thread_wake = wake.clone();
    let thread = crate::task_spawn::named_thread("harn-session-wal-watch".to_string(), move || {
        let _exit = WatchExit {
            path: thread_path.clone(),
            stop: cleanup_stop,
        };
        watch_loop(
            thread_path,
            reader,
            initial_version,
            thread_stop,
            thread_wake,
            events,
        );
    })
    .expect("start session WAL watcher");
    let thread_id = thread.thread().id();
    running.push((
        path,
        Arc::new(WatchState {
            stop,
            wake,
            thread_id,
            thread: Mutex::new(Some(thread)),
            #[cfg(test)]
            joining: Mutex::new(None),
        }),
    ));
}

fn watch_loop(
    path: PathBuf,
    reader: harn_session_store::wal_watch::WatchReader,
    mut last_version: i64,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<WatchSignal>,
    rx: mpsc::Receiver<WatchSignal>,
) {
    let mut watcher = match notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
        let _ = tx.send(WatchSignal::Wake);
    }) {
        Ok(watcher) => watcher,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
    }
    let wal = wal_sidecar_path(&path);
    if wal.exists() {
        let _ = watcher.watch(&wal, RecursiveMode::NonRecursive);
    }

    while !stop.load(Ordering::Relaxed) {
        let signal = match rx.recv_timeout(POLL) {
            Ok(signal) => Some(signal),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        poll_once(&path, &reader, &mut last_version);
        if let Some(signal) = signal {
            signal.polled();
        }
    }
}

/// One watch step, with no thread or timer: read `data_version` and, when
/// another connection has committed since `last_version`, publish every
/// metadata snapshot that moved. Returns whether a new commit was observed.
///
/// `data_version` is per connection, so `last_version` is only meaningful
/// against the same `reader`.
fn poll_once(path: &Path, reader: &rusqlite::Connection, last_version: &mut i64) -> bool {
    let Ok(version) = data_version(reader) else {
        return false;
    };
    if version == *last_version {
        super::session_change::retry_pending(path);
        return false;
    }
    *last_version = version;
    publish_session_changes(path, reader);
    super::session_change::retry_pending(path);
    true
}

fn publish_session_changes(path: &Path, reader: &rusqlite::Connection) {
    let Ok(snapshots) = list_title_snapshots(reader) else {
        return;
    };
    for snapshot in snapshots {
        // Keep the store owner's identity. The fanout resolves the current
        // committed row rather than trusting an earlier watcher snapshot.
        super::session_change::dispatch_foreign(path, &snapshot.id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;

    use harn_clock::test_support::recv_within;
    use harn_session_store::{
        CreateSession, SessionChangeObserver, SessionMeta, SessionStore, UpdateSession,
    };
    use tempfile::TempDir;

    use crate::{open_canonical_store, subscribe_session_changes};

    struct Recording(mpsc::Sender<String>);

    impl SessionChangeObserver for Recording {
        fn session_updated(&self, meta: &SessionMeta) {
            let _ = self.0.send(meta.title.clone().unwrap_or_default());
        }
    }

    #[tokio::test]
    async fn subscription_reconciliation_uses_the_current_census() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().unwrap();
        let store = open_canonical_store(root.path()).unwrap();
        store
            .create(CreateSession {
                id: Some("replacement-census".into()),
                title: Some("before".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let stale_census =
            super::super::session_change::current_observer(Some(store.path())).is_some();
        assert!(!stale_census, "measure the actual empty observer registry");
        assert_eq!(super::watcher_count_for(store.path()), 0);
        let (delivered, delivery) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(delivered)));
        assert!(super::super::session_change::current_observer(Some(store.path())).is_some());
        assert_eq!(super::watcher_count_for(store.path()), 1);
        super::sync_watchers();
        assert_eq!(
            super::watcher_count_for(store.path()),
            1,
            "an earlier empty census must not stop the replacement watcher"
        );
        let foreign = rusqlite::Connection::open(store.path()).unwrap();
        foreign
            .execute(
                "UPDATE sessions SET title = 'after' WHERE id = 'replacement-census'",
                [],
            )
            .unwrap();
        super::flush_watcher(store.path());
        assert_eq!(
            recv_within("replacement observed the foreign title", &delivery),
            "after"
        );
    }

    #[tokio::test]
    async fn a_reopened_store_survives_an_earlier_close_decision() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().unwrap();
        let old = open_canonical_store(root.path()).unwrap();
        let database = old.path().to_owned();
        old.create(CreateSession {
            id: Some("reopened".into()),
            title: Some("before".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        drop(old);
        assert!(!super::registered()
            .iter()
            .any(|(path, _)| path == &database));
        let replacement = open_canonical_store(root.path()).unwrap();
        let (delivered, delivery) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(delivered)));
        assert!(super::registered()
            .iter()
            .any(|(path, count)| path == &database && *count == 1));
        assert_eq!(super::watcher_count_for(&database), 1);
        // Drive the path-stop decision an older last-handle drop can carry
        // after releasing REGISTERED and before obtaining WATCHERS.
        super::sync_watchers();
        assert_eq!(
            super::watcher_count_for(&database),
            1,
            "the replacement still owns this store path"
        );
        let foreign = rusqlite::Connection::open(&database).unwrap();
        foreign
            .execute(
                "UPDATE sessions SET title = 'after' WHERE id = 'reopened'",
                [],
            )
            .unwrap();
        super::flush_watcher(&database);
        assert_eq!(
            recv_within("reopened store retained foreign delivery", &delivery),
            "after"
        );
        drop(foreign);
        drop(replacement);
        assert_eq!(super::watcher_count_for(&database), 0);
        assert!(!super::wal_sidecar_path(&database).exists());
    }

    /// Detection without a thread or a clock: a foreign commit is published
    /// by exactly the next poll, and a poll with nothing new publishes
    /// nothing.
    #[tokio::test]
    async fn one_poll_publishes_a_foreign_title_commit() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let database = {
            let store = open_canonical_store(root.path()).expect("open canonical store");
            store
                .create(CreateSession {
                    id: Some("polled".to_string()),
                    title: Some("before".to_string()),
                    ..CreateSession::default()
                })
                .await
                .expect("create");
            store.path().to_path_buf()
        };

        // No handle is open, so subscribing starts no watcher for this store:
        // the only thing that can publish below is the direct poll.
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));
        assert_eq!(super::watcher_count_for(&database), 0);

        let reader =
            harn_session_store::wal_watch::open_watch_reader(&database).expect("watch reader");
        let mut last_version =
            harn_session_store::wal_watch::data_version(&reader).expect("data version");
        super::seed_session_memory(&database, &reader);
        assert!(
            !super::poll_once(&database, &reader, &mut last_version),
            "no commit since the baseline"
        );

        let foreign = rusqlite::Connection::open(&database).expect("foreign writer");
        foreign
            .execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                rusqlite::params!["after", "polled"],
            )
            .expect("foreign rename");

        assert!(super::poll_once(&database, &reader, &mut last_version));
        assert_eq!(rx.try_recv().as_deref(), Ok("after"));
        assert!(!super::poll_once(&database, &reader, &mut last_version));
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "a quiet poll publishes nothing"
        );
    }

    /// Independent database copies may carry the same session id and metadata.
    #[tokio::test]
    async fn identical_session_ids_in_distinct_databases_publish_independently() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let roots = [TempDir::new().unwrap(), TempDir::new().unwrap()];
        let mut readers = Vec::new();
        for root in &roots {
            let store = open_canonical_store(root.path()).expect("canonical store");
            store
                .create(CreateSession {
                    id: Some("shared-id".into()),
                    title: Some("stable".into()),
                    ..Default::default()
                })
                .await
                .expect("create");
            let writer = rusqlite::Connection::open(store.path()).unwrap();
            writer.execute("UPDATE sessions SET created_at_ms = 1, created_at = '1970-01-01T00:00:00.001Z', updated_at_ms = 1, updated_at = '1970-01-01T00:00:00.001Z'", [])
                .expect("equal timestamps in independent databases");
            let reader =
                harn_session_store::wal_watch::open_watch_reader(store.path()).expect("reader");
            let version = harn_session_store::wal_watch::data_version(&reader).unwrap();
            readers.push((store.path().to_owned(), reader, version));
        }
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));
        assert_eq!(
            harn_session_store::wal_watch::describe_session(&readers[0].1, "shared-id").unwrap(),
            harn_session_store::wal_watch::describe_session(&readers[1].1, "shared-id").unwrap(),
            "the metadata must be identical or a full-snapshot cache can pass vacuously"
        );
        for (database, reader, _) in &readers {
            super::seed_session_memory(database, reader);
        }
        for (database, reader, version) in &mut readers {
            let foreign = rusqlite::Connection::open(&*database).expect("foreign writer");
            foreign
                .execute(
                    "UPDATE sessions SET usage_input = 7 WHERE id = 'shared-id'",
                    [],
                )
                .expect("foreign commit");
            assert!(
                super::poll_once(database, reader, version),
                "the database committed"
            );
            assert_eq!(
                rx.try_recv().as_deref(),
                Ok("stable"),
                "each database must publish its own identical metadata change"
            );
            assert!(!super::poll_once(database, reader, version));
            assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        }
    }

    /// A foreign metadata commit reaches the owning poll without a title change.
    #[tokio::test]
    async fn foreign_usage_change_publishes_once_when_title_is_unchanged() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let database = {
            let store = open_canonical_store(root.path()).expect("canonical store");
            store
                .create(CreateSession {
                    id: Some("foreign-usage".into()),
                    title: Some("stable".into()),
                    ..Default::default()
                })
                .await
                .expect("create");
            store.path().to_owned()
        };
        struct Metadata(mpsc::Sender<SessionMeta>);
        impl SessionChangeObserver for Metadata {
            fn session_updated(&self, meta: &SessionMeta) {
                let _ = self.0.send(meta.clone());
            }
        }
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Metadata(tx)));
        let reader = harn_session_store::wal_watch::open_watch_reader(&database).expect("reader");
        let mut version = harn_session_store::wal_watch::data_version(&reader).expect("version");
        super::seed_session_memory(&database, &reader);
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        let foreign = rusqlite::Connection::open(&database).expect("foreign writer");
        foreign
            .execute(
                "UPDATE sessions SET usage_input = 7 WHERE id = 'foreign-usage'",
                [],
            )
            .expect("foreign metadata commit");
        assert!(super::poll_once(&database, &reader, &mut version));
        let delivered = rx.try_recv().expect("same-title foreign change delivered");
        assert_eq!(delivered.title.as_deref(), Some("stable"));
        assert_eq!(delivered.usage_input, 7);
        assert!(!super::poll_once(&database, &reader, &mut version));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    /// The same path through the real watcher thread and filesystem watcher.
    #[tokio::test]
    async fn foreign_title_write_reaches_a_subscriber_in_this_process() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let store = open_canonical_store(root.path()).expect("open canonical store");
        store
            .create(CreateSession {
                id: Some("watched".to_string()),
                title: Some("before".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");

        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));

        // A second connection commits the way another process would: no hook.
        let foreign = rusqlite::Connection::open(store.path()).expect("foreign writer");
        foreign
            .execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                rusqlite::params!["after", "watched"],
            )
            .expect("foreign rename");

        let title = recv_within("watcher publishing the foreign title", &rx);
        assert_eq!(title, "after");
    }

    /// The claim on watching a path belongs to the open handle. Before that
    /// was true, `register_store_path` only ever appended, so this count
    /// stayed at one after the drop and every later subscription reattached a
    /// reader and a thread to a database nobody had open.
    #[tokio::test]
    async fn a_closed_store_stops_being_watched() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let (tx, _rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));

        let store = open_canonical_store(root.path()).expect("open canonical store");
        let database = store.path().to_path_buf();
        assert_eq!(
            super::watcher_count_for(&database),
            1,
            "a store held open under a live subscription is watched",
        );
        let wal = super::wal_sidecar_path(&database);
        assert!(wal.exists(), "the open store and reader hold a WAL file");

        drop(store);
        assert_eq!(
            super::watcher_count_for(&database),
            0,
            "the last handle to close takes the watcher with it",
        );
        assert!(
            !wal.exists(),
            "the last handle waits until its reader closes"
        );
    }

    #[tokio::test]
    async fn a_foreign_delivery_can_remove_the_last_subscription() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().unwrap();
        let store = open_canonical_store(root.path()).unwrap();
        store
            .create(CreateSession {
                id: Some("self-unsubscribe".into()),
                title: Some("before".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        struct SelfRemoving {
            subscription: std::sync::Mutex<Option<crate::SessionChangeSubscription>>,
            removed: mpsc::Sender<bool>,
            resume: std::sync::Mutex<mpsc::Receiver<()>>,
        }
        impl SessionChangeObserver for SelfRemoving {
            fn session_updated(&self, meta: &SessionMeta) {
                assert_eq!(
                    std::thread::current().name(),
                    Some("harn-session-wal-watch")
                );
                assert_eq!(meta.title.as_deref(), Some("after"));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    drop(self.subscription.lock().unwrap().take());
                }));
                self.removed.send(result.is_ok()).unwrap();
                recv_within(
                    "release the retiring watcher callback",
                    &self.resume.lock().unwrap(),
                );
            }
        }
        let (removed, rx) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let observer = Arc::new(SelfRemoving {
            subscription: std::sync::Mutex::new(None),
            removed,
            resume: std::sync::Mutex::new(resumed),
        });
        *observer.subscription.lock().unwrap() = Some(subscribe_session_changes(observer.clone()));
        let foreign = rusqlite::Connection::open(store.path()).unwrap();
        foreign
            .execute(
                "UPDATE sessions SET title = 'after' WHERE id = 'self-unsubscribe'",
                [],
            )
            .unwrap();
        assert!(recv_within(
            "last subscription removed on the watcher thread",
            &rx
        ));
        drop(foreign);
        assert_eq!(
            super::watcher_count_for(store.path()),
            1,
            "the retiring reader must remain owned while its callback is blocked"
        );
        let database = store.path().to_owned();
        let (joining, join_started) = mpsc::channel();
        let state = super::watchers()
            .iter()
            .find(|(seen, _)| seen == &database)
            .map(|(_, state)| state.clone())
            .unwrap();
        *state.joining.lock().unwrap() = Some(joining);
        let (closed, store_closed) = mpsc::channel();
        let close = crate::runtime_stack::spawn(move || {
            drop(store);
            closed.send(()).unwrap();
        });
        recv_within(
            "external store close reached the retiring thread join",
            &join_started,
        );
        assert!(super::wal_sidecar_path(&database).exists());
        resume.send(()).unwrap();
        recv_within(
            "external store close waited for the callback and reader",
            &store_closed,
        );
        close.join().unwrap();
        assert_eq!(super::watcher_count_for(&database), 0);
        assert!(!super::wal_sidecar_path(&database).exists());
    }

    #[tokio::test]
    async fn resubscribing_while_a_foreign_callback_retires_keeps_watching() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().unwrap();
        let store = open_canonical_store(root.path()).unwrap();
        store
            .create(CreateSession {
                id: Some("resubscribe".into()),
                title: Some("before".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        struct Replacing {
            subscription: std::sync::Mutex<Option<crate::SessionChangeSubscription>>,
            replacement: Arc<dyn SessionChangeObserver>,
            replaced: mpsc::Sender<()>,
            resume: std::sync::Mutex<mpsc::Receiver<()>>,
        }
        impl SessionChangeObserver for Replacing {
            fn session_updated(&self, meta: &SessionMeta) {
                assert_eq!(
                    std::thread::current().name(),
                    Some("harn-session-wal-watch")
                );
                assert_eq!(meta.title.as_deref(), Some("after"));
                let mut subscription = self.subscription.lock().unwrap();
                drop(subscription.take());
                *subscription = Some(subscribe_session_changes(self.replacement.clone()));
                self.replaced.send(()).unwrap();
                recv_within(
                    "release the replaced watcher callback",
                    &self.resume.lock().unwrap(),
                );
            }
        }
        let (delivered, delivery) = mpsc::channel();
        let (replaced, replacement_ready) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let observer = Arc::new(Replacing {
            subscription: std::sync::Mutex::new(None),
            replacement: Arc::new(Recording(delivered)),
            replaced,
            resume: std::sync::Mutex::new(resumed),
        });
        *observer.subscription.lock().unwrap() = Some(subscribe_session_changes(observer.clone()));
        let foreign = rusqlite::Connection::open(store.path()).unwrap();
        foreign
            .execute(
                "UPDATE sessions SET title = 'after' WHERE id = 'resubscribe'",
                [],
            )
            .unwrap();
        recv_within(
            "replacement subscribed while the old callback is blocked",
            &replacement_ready,
        );
        assert_eq!(
            super::watcher_count_for(store.path()),
            2,
            "one active and one retiring reader"
        );
        foreign
            .execute(
                "UPDATE sessions SET title = 'replacement' WHERE id = 'resubscribe'",
                [],
            )
            .unwrap();
        resume.send(()).unwrap();
        super::flush_watcher(store.path());
        assert_eq!(
            recv_within(
                "replacement received the subsequent foreign commit",
                &delivery
            ),
            "replacement"
        );
        assert!(matches!(
            delivery.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(observer.subscription.lock().unwrap().take());
        drop(foreign);
        let database = store.path().to_owned();
        drop(store);
        assert_eq!(super::watcher_count_for(&database), 0);
        assert!(!super::wal_sidecar_path(&database).exists());
    }

    #[tokio::test]
    async fn a_failed_local_read_is_retried_without_another_data_version_change() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().unwrap();
        let database = {
            let store = open_canonical_store(root.path()).unwrap();
            store
                .create(CreateSession {
                    id: Some("unreadable".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            store.path().to_owned()
        };
        let (delivered, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(delivered)));
        struct UnreadableDuringHook(std::path::PathBuf);
        impl SessionChangeObserver for UnreadableDuringHook {
            fn session_updated(&self, meta: &SessionMeta) {
                let migration = rusqlite::Connection::open(&self.0).unwrap();
                migration
                    .execute("ALTER TABLE sessions RENAME TO unavailable_sessions", [])
                    .unwrap();
                assert!(
                    harn_session_store::wal_watch::describe_session(&migration, &meta.id).is_err()
                );
                crate::stdlib::session_change::dispatch(&self.0, meta);
                migration
                    .execute("ALTER TABLE unavailable_sessions RENAME TO sessions", [])
                    .unwrap();
                assert_eq!(
                    harn_session_store::wal_watch::describe_session(&migration, &meta.id)
                        .unwrap()
                        .title,
                    meta.title,
                    "the same committed row is available again"
                );
            }
        }
        let writer = harn_session_store::SqliteSessionStore::open_with_hooks(
            &database,
            harn_session_store::StoreHooks {
                change_observer: Some(Arc::new(UnreadableDuringHook(database.clone()))),
                ..Default::default()
            },
        )
        .unwrap();
        writer
            .update(
                "unreadable",
                UpdateSession {
                    title: Some("committed while unreadable".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        let reader = super::open_watch_reader(&database).unwrap();
        let mut version = super::data_version(&reader).unwrap();
        assert!(
            !super::poll_once(&database, &reader, &mut version),
            "data_version is unchanged"
        );
        assert_eq!(rx.try_recv().as_deref(), Ok("committed while unreadable"));
        assert!(!super::poll_once(&database, &reader, &mut version));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn watcher_first_local_commit_publishes_once_and_preserves_other_metadata() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let store = open_canonical_store(root.path()).expect("canonical store");
        store
            .create(CreateSession {
                id: Some("watcher-first".into()),
                title: Some("before".into()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));
        super::flush_watcher(store.path());

        struct WatcherFirst {
            path: std::path::PathBuf,
            fanout: harn_session_store::SharedSessionChangeObserver,
        }
        impl SessionChangeObserver for WatcherFirst {
            fn session_updated(&self, meta: &SessionMeta) {
                // Exercise the actual commit-to-hook window deterministically.
                // The writer has released SQLite's lock before this callback.
                super::flush_watcher(&self.path);
                self.fanout.session_updated(meta);
            }
        }
        let writer = harn_session_store::SqliteSessionStore::open_with_hooks(
            store.path(),
            harn_session_store::StoreHooks {
                change_observer: Some(Arc::new(WatcherFirst {
                    path: store.path().to_owned(),
                    fanout: crate::stdlib::session_change::current_observer(Some(store.path()))
                        .expect("live fanout"),
                })),
                ..Default::default()
            },
        )
        .expect("writer sharing canonical database");
        writer
            .update(
                "watcher-first",
                UpdateSession {
                    title: Some("after".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("rename");
        assert_eq!(rx.try_recv().as_deref(), Ok("after"));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));

        let updated = writer
            .update(
                "watcher-first",
                UpdateSession {
                    usage_input: Some(7),
                    ..Default::default()
                },
            )
            .await
            .expect("usage update with unchanged title");
        assert_eq!(updated.usage_input, 7);
        assert_eq!(rx.try_recv().as_deref(), Ok("after"));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn local_update_does_not_double_publish_through_the_watcher() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        #[cfg(unix)]
        let alias_parent = TempDir::new().expect("alias parent");
        #[cfg(unix)]
        let root_path = {
            let alias = alias_parent.path().join("store-root");
            std::os::unix::fs::symlink(root.path(), &alias).expect("aliased store root");
            alias
        };
        #[cfg(not(unix))]
        let root_path = root.path().to_owned();
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));
        let store = open_canonical_store(&root_path).expect("open canonical store");
        store
            .create(CreateSession {
                id: Some("local".to_string()),
                title: Some("before".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");
        // Make the watcher see "before" on its own. Otherwise it can fold the
        // create and the rename into one poll, meet the session first as
        // "after", and stay silent whether or not the hook deduplicates, which
        // would make the assertion below vacuous.
        super::flush_watcher(store.path());

        store
            .update(
                "local",
                UpdateSession {
                    title: Some("after".to_string()),
                    ..UpdateSession::default()
                },
            )
            .await
            .expect("local rename");

        let first = recv_within("in-process hook publishing the rename", &rx);
        assert_eq!(first, "after");
        // Wait until the watcher has polled past the rename's commit, so a
        // republish would already be on the channel, rather than hoping a
        // quiet period was long enough for it to look.
        super::flush_watcher(store.path());
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "watcher must not republish a title the in-process hook already sent"
        );
    }
}
