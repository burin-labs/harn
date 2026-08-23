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
    thread: JoinHandle<()>,
}

static REGISTERED: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());
static WATCHERS: RwLock<Vec<(PathBuf, WatchState)>> = RwLock::new(Vec::new());

fn registered() -> std::sync::RwLockWriteGuard<'static, Vec<PathBuf>> {
    REGISTERED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn watchers() -> std::sync::RwLockWriteGuard<'static, Vec<(PathBuf, WatchState)>> {
    WATCHERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Remember a canonical store path so a later subscription can watch it.
pub(super) fn register_store_path(path: &Path) {
    if path == Path::new(":memory:") {
        return;
    }
    let path = path.to_path_buf();
    let mut paths = registered();
    if !paths.iter().any(|seen| seen == &path) {
        paths.push(path);
    }
}

/// Start or stop watchers to match whether anyone is subscribed.
pub(super) fn sync_watchers(subscribers_live: bool) {
    if subscribers_live {
        let paths = registered().clone();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            start_watcher(path);
        }
    } else {
        let running = std::mem::take(&mut *watchers());
        for (_path, state) in running {
            state.stop.store(true, Ordering::Relaxed);
            let _ = state.thread.join();
        }
    }
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
    let thread = thread::Builder::new()
        .name("harn-session-wal-watch".to_string())
        .spawn(move || watch_loop(thread_path, reader, initial_version, thread_stop))
        .expect("start session WAL watcher");
    running.push((path, WatchState { stop, thread }));
}

fn watch_loop(
    path: PathBuf,
    reader: rusqlite::Connection,
    mut last_version: i64,
    stop: Arc<AtomicBool>,
) {
    let (tx, rx) = mpsc::channel();
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

    #[tokio::test]
    async fn local_update_does_not_double_publish_through_the_watcher() {
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
