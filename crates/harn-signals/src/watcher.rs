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
    // Hex keeps any session id a safe file name.
    let name: String = session_id
        .bytes()
        .take(120)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    take(watchers.join(format!("{name}.lock")))
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

/// Remove every watcher lock nobody holds: a watcher killed before its
/// destructor ran leaves one.
pub(crate) fn sweep_orphans(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir.join(WATCHERS_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lock") {
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
