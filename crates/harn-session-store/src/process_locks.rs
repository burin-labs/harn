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

/// A stable identity for `path`: symlinks are followed even when they dangle,
/// so the key does not change when SQLite later creates the target.
fn key(path: &Path) -> PathBuf {
    let mut current = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    for _ in 0..40 {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            return canonical;
        }
        let Ok(target) = std::fs::read_link(&current) else {
            break;
        };
        current = match current.parent() {
            Some(parent) => parent.join(target),
            None => target,
        };
    }
    match (current.parent(), current.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or(current),
        _ => current,
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

        let guard = register(&link);
        std::fs::write(&target, b"").expect("create the target");
        assert!(lock().holds(&link), "the symlink spelling still matches");
        assert!(lock().holds(&link));
        assert!(lock().holds(&target));
        drop(guard);
        assert!(!lock().holds(&link));
    }
}
