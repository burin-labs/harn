//! Which database files this process currently holds SQLite connections on.
//!
//! POSIX drops every advisory lock a process holds on a file when any
//! descriptor for that file closes. Copying a live database, `-wal` or `-shm`
//! with plain file handles is therefore only safe while this process holds no
//! connection on it; otherwise the close releases SQLite's in-use lock on the
//! shared WAL index and the next process to open the store truncates it under
//! the live mappings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static HELD: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

pub(crate) struct HeldPaths(MutexGuard<'static, BTreeMap<PathBuf, usize>>);

impl HeldPaths {
    pub(crate) fn holds(&self, path: &Path) -> bool {
        self.0.contains_key(&key(path))
    }
}

/// Lock the registry. Hold it across a raw copy so no connection can open
/// (and register) on the file until the copy has closed its descriptors.
pub(crate) fn lock() -> HeldPaths {
    HeldPaths(HELD.lock().unwrap_or_else(|error| error.into_inner()))
}

/// Registration of one open connection; drop it after the connection closes.
pub struct HeldGuard(PathBuf);

impl HeldGuard {
    /// Re-key the registration once the connection has created its file. A
    /// dangling symlink resolves to a different identity after SQLite creates
    /// the target, and lookups canonicalize the existing target.
    pub(crate) fn rekey(&mut self, path: &Path) {
        let new = key(path);
        if new == self.0 {
            return;
        }
        let mut held = lock().0;
        *held.entry(new.clone()).or_default() += 1;
        if let Some(count) = held.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.0);
            }
        }
        self.0 = new;
    }
}

/// Register before opening the connection so a concurrent raw copy either
/// sees the registration or finishes first.
pub(crate) fn register(path: &Path) -> HeldGuard {
    let key = key(path);
    *lock().0.entry(key.clone()).or_default() += 1;
    HeldGuard(key)
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        let mut held = lock().0;
        if let Some(count) = held.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.0);
            }
        }
    }
}

fn key(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_dangling_symlink_keeps_its_identity_once_the_target_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link = dir.path().join("link.sqlite");
        let target = dir.path().join("real.sqlite");
        std::os::unix::fs::symlink(&target, &link).expect("dangling symlink");

        let mut guard = register(&link);
        std::fs::write(&target, b"").expect("create the target");
        assert!(!lock().holds(&link), "the stale spelling no longer matches");
        guard.rekey(&link);
        assert!(lock().holds(&link));
        assert!(lock().holds(&target));
        drop(guard);
        assert!(!lock().holds(&link));
    }
}
