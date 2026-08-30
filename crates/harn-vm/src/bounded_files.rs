//! Shared retention for bounded local evidence directories.

use std::path::Path;
use std::sync::Mutex;

static RETENTION_MUTEX: Mutex<()> = Mutex::new(());

/// Serialize one write-and-prune transaction across threads and processes.
/// The lock lives in Harn's runtime lock root, not inside the evidence
/// directory, so it cannot be mistaken for a retained artifact.
pub(crate) fn with_retention_transaction<T>(
    parent: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // `flock` is per open file description on Unix and per handle on Windows.
    // Serialize same-process contenders first so only one handle from this
    // process can wait on the cross-process lock. Every caller takes these in
    // the same order.
    let _process_guard = RETENTION_MUTEX
        .lock()
        .map_err(|_| "evidence retention process lock was poisoned".to_string())?;
    let lock_identity = parent.join(".harn-evidence-retention");
    let _lock = crate::conditional_replace::acquire_lock(&lock_identity)
        .map_err(|error| format!("failed to lock evidence retention: {error}"))?;
    operation()
}

pub(crate) fn retain_newest_files(
    parent: &Path,
    keep_path: &Path,
    retain_files: usize,
    matches: impl Fn(&Path) -> bool,
) -> Result<(), String> {
    if retain_files == usize::MAX {
        return Ok(());
    }
    let mut files = std::fs::read_dir(parent)
        .map_err(|error| format!("failed to list evidence artifacts: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !entry.file_type().ok()?.is_file() || !matches(&path) {
                return None;
            }
            Some((entry.metadata().ok()?.modified().ok()?, path))
        })
        .collect::<Vec<_>>();
    files.sort_by(
        |left, right| match (left.1 == keep_path, right.1 == keep_path) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)),
        },
    );
    for (_, path) in files.into_iter().skip(retain_files.max(1)) {
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!(
                    "failed to prune evidence artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_write_and_prune_transactions_keep_the_exact_bound() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(12));
        let writers = (0..12)
            .map(|index| {
                let parent = parent.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let path = parent.join(format!("hxe-{index:02}.json"));
                    with_retention_transaction(&parent, || {
                        std::fs::write(&path, b"{}")
                            .map_err(|error| format!("write failed: {error}"))?;
                        retain_newest_files(&parent, &path, 4, |candidate| {
                            candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
                        })
                    })
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }

        let retained = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
            .count();
        assert_eq!(retained, 4);
    }
}
