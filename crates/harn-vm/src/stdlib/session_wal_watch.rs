//! Cross-process session-title watch via WAL + `PRAGMA data_version`.
//!
//! One thread per database path. The thread keeps its own reader so it never
//! takes the store mutex, and it only publishes when a title or pin actually
//! moved. Local writes still go through [`super::session_change`]; the
//! fingerprint cache there drops the duplicate when this thread later sees
//! the same commit.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use harn_session_store::wal_watch::{
    data_version, describe_session, list_title_snapshots, open_watch_reader, wal_sidecar_path,
};
use notify::{RecursiveMode, Watcher};

use super::session_change::{remember_title, TitleMemory};

const POLL: Duration = Duration::from_millis(250);

struct WatchState {
    stop: Arc<AtomicBool>,
    /// Wakes the loop out of its poll wait so a stop is observed at once.
    /// Without it, joining costs up to one whole [`POLL`] interval, and a
    /// store handle closing on a hot path would pay that every time.
    wake: mpsc::Sender<notify::Result<notify::Event>>,
    thread: JoinHandle<()>,
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
static WATCHERS: RwLock<Vec<(PathBuf, WatchState)>> = RwLock::new(Vec::new());

fn registered() -> std::sync::RwLockWriteGuard<'static, Vec<(PathBuf, usize)>> {
    REGISTERED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn watchers() -> std::sync::RwLockWriteGuard<'static, Vec<(PathBuf, WatchState)>> {
    WATCHERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One open handle's claim on watching a canonical store path.
///
/// Held by the store handle itself, so the path stays watchable for exactly as
/// long as something in this process has the store open. The last claim to
/// drop also stops the running watcher, which closes its reader before the
/// caller can observe the directory again.
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
        let released = {
            let mut paths = registered();
            match paths.iter().position(|(seen, _)| seen == &path) {
                Some(index) => {
                    paths[index].1 = paths[index].1.saturating_sub(1);
                    if paths[index].1 == 0 {
                        paths.remove(index);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };
        if released {
            stop_watcher(&path);
        }
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

/// Start or stop watchers to match whether anyone is subscribed.
pub(super) fn sync_watchers(subscribers_live: bool) {
    if subscribers_live {
        let paths: Vec<PathBuf> = registered().iter().map(|(path, _)| path.clone()).collect();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            start_watcher(path);
        }
    } else {
        let running = std::mem::take(&mut *watchers());
        for (_path, state) in running {
            stop_and_join(state);
        }
    }
}

/// Stop and join the watcher for one path, if it is running.
///
/// Joining rather than signalling and returning is the point: the reader the
/// thread owns is what creates the SQLite sidecars, so a caller that drops its
/// last handle and then reads the directory must not race the close.
fn stop_watcher(path: &Path) {
    let state = {
        let mut running = watchers();
        running
            .iter()
            .position(|(seen, _)| seen == path)
            .map(|index| running.remove(index).1)
    };
    if let Some(state) = state {
        stop_and_join(state);
    }
}

/// Signal one watcher and wait for its reader to close.
///
/// The wake is a send rather than a channel drop: the loop's own filesystem
/// callback holds the other sender, so closing this one would not disconnect
/// the receiver and the join would still wait out a poll interval.
fn stop_and_join(state: WatchState) {
    state.stop.store(true, Ordering::Relaxed);
    let _ = state.wake.send(Ok(notify::Event::default()));
    let _ = state.thread.join();
}

/// How many watchers are running for `path`. Test-only: the invariant this
/// module now holds is about the watcher set, so a case should assert on that
/// set rather than on the SQLite sidecars it happens to leave behind.
#[cfg(test)]
fn watcher_count_for(path: &Path) -> usize {
    watchers().iter().filter(|(seen, _)| seen == path).count()
}

fn start_watcher(path: PathBuf) {
    let mut running = watchers();
    if running.iter().any(|(seen, _)| seen == &path) {
        return;
    }
    let Ok(reader) = open_watch_reader(&path) else {
        return;
    };
    let Ok(initial_version) = data_version(&reader) else {
        return;
    };
    if let Ok(snapshots) = list_title_snapshots(&reader) {
        for snapshot in snapshots {
            remember_title(
                &snapshot.id,
                snapshot.title.as_deref(),
                snapshot.title_pinned,
            );
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_path = path.clone();
    let (wake, events) = mpsc::channel();
    let thread_wake = wake.clone();
    let thread = thread::Builder::new()
        .name("harn-session-wal-watch".to_string())
        .spawn(move || {
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
    running.push((path, WatchState { stop, wake, thread }));
}

fn watch_loop(
    path: PathBuf,
    reader: rusqlite::Connection,
    mut last_version: i64,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<notify::Result<notify::Event>>,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
) {
    let mut watcher = match notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
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
        match rx.recv_timeout(POLL) {
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let Ok(version) = data_version(&reader) else {
            continue;
        };
        if version == last_version {
            continue;
        }
        last_version = version;
        publish_title_changes(&reader);
    }
}

fn publish_title_changes(reader: &rusqlite::Connection) {
    let Ok(snapshots) = list_title_snapshots(reader) else {
        return;
    };
    for snapshot in snapshots {
        if remember_title(
            &snapshot.id,
            snapshot.title.as_deref(),
            snapshot.title_pinned,
        ) != TitleMemory::Changed
        {
            continue;
        }
        if let Ok(meta) = describe_session(reader, &snapshot.id) {
            super::session_change::dispatch(&meta);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

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

        let title = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("watcher published the foreign title");
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

        drop(store);
        assert_eq!(
            super::watcher_count_for(&database),
            0,
            "the last handle to close takes the watcher with it",
        );
    }

    #[tokio::test]
    async fn local_update_does_not_double_publish_through_the_watcher() {
        let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
        let root = TempDir::new().expect("root");
        let (tx, rx) = mpsc::channel();
        let _subscription = subscribe_session_changes(Arc::new(Recording(tx)));
        let store = open_canonical_store(root.path()).expect("open canonical store");
        store
            .create(CreateSession {
                id: Some("local".to_string()),
                title: Some("before".to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create");

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

        let first = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("in-process hook published");
        assert_eq!(first, "after");
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "watcher must not republish a title the in-process hook already sent"
        );
    }
}
