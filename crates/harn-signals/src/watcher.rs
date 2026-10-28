//! One idle watcher per session: a lock file under `watchers/` that its
//! holder removes as it exits.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::inbox::create_private_dir;
use crate::SignalsError;

const WATCHERS_DIR: &str = "watchers";

/// A held watcher lock. Dropping it removes the lock file while the lock is
/// still held, so no watcher file outlives its watcher.
pub struct WatcherLock {
    path: PathBuf,
    _file: File,
}

impl Drop for WatcherLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn claim(dir: &Path, session_id: &str) -> Result<Option<WatcherLock>, SignalsError> {
    let watchers = dir.join(WATCHERS_DIR);
    create_private_dir(&watchers)?;
    take(watchers.join(lock_name(session_id)))
}

/// Hex keeps any session id a safe file name.
fn lock_name(session_id: &str) -> String {
    let bytes = session_id.as_bytes();
    format!("{}.lock", hex::encode(&bytes[..bytes.len().min(120)]))
}

/// Lock `path` without waiting. A holder, or a sweep, removes the file as it
/// releases it, so a lock taken on a file removed in between guards nothing:
/// retry on the current file.
fn take(path: PathBuf) -> Result<Option<WatcherLock>, SignalsError> {
    for _ in 0..3 {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| SignalsError::io(&path, source))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(source)) => return Err(SignalsError::io(&path, source)),
        }
        if same_file(&file, &path) {
            return Ok(Some(WatcherLock { path, _file: file }));
        }
    }
    Ok(None)
}

/// Remove the watcher locks nobody holds for sessions no longer registered:
/// a watcher killed before its destructor ran leaves one. A registered
/// session's lock is never touched, so a sweep cannot race a claim.
pub(crate) fn sweep_orphans(dir: &Path, sessions: &[String]) {
    let Ok(entries) = fs::read_dir(dir.join(WATCHERS_DIR)) else {
        return;
    };
    let live: Vec<String> = sessions.iter().map(|id| lock_name(id)).collect();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".lock") && !live.iter().any(|live| *live == name) {
            // Taking the lock and dropping it removes the file.
            let _ = take(path);
        }
    }
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(current)) => held.dev() == current.dev() && held.ino() == current.ino(),
        _ => false,
    }
}

/// Windows refuses to remove a file another process has open, so the file a
/// lock was taken on is always the one at the path.
#[cfg(not(unix))]
fn same_file(_file: &File, path: &Path) -> bool {
    path.exists()
}
