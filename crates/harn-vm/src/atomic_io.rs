//! Atomic file write helpers.
//!
//! All persistent on-disk state in Harn (workflow mailboxes, run records,
//! event logs, lockfiles, package manifests, ...) should use these helpers
//! rather than `std::fs::write` so that concurrent readers and abrupt
//! process termination cannot observe a half-written file.
//!
//! The pattern is:
//!
//! 1. Create the parent directory if needed.
//! 2. Write to a sibling `.<name>.<uuid>.tmp` file.
//! 3. `fsync` the temp file.
//! 4. `rename` the temp file over the destination (atomic on POSIX, atomic
//!    overwrite on Windows since Rust 1.5+).
//! 5. Best-effort `fsync` the parent directory so the rename survives a
//!    power loss on filesystems that decouple the dirent from the inode.
//!
//! On any failure between (2) and (4), the temp file is removed so that
//! repeated retries don't leak `.tmp` siblings.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Atomically write `bytes` to `path`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |writer| writer.write_all(bytes))
}

/// Atomically write the destination at `path` by streaming through a
/// `BufWriter`. The closure runs against a buffered writer over a sibling
/// temp file. On success, the buffer is flushed, the file is `fsync`'d, and
/// the temp file is renamed over `path`.
///
/// Use this for line-by-line or chunked writes (e.g. JSONL compaction).
/// For a one-shot byte write, prefer [`atomic_write`].
pub fn atomic_write_with<F>(path: &Path, write_fn: F) -> io::Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let tmp = TempFile::create(path)?;
    let result = write_and_finalize(&tmp, write_fn);
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp.path);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp.path, path) {
        let _ = std::fs::remove_file(&tmp.path);
        return Err(err);
    }
    sync_parent_dir(path);
    Ok(())
}

fn write_and_finalize<F>(tmp: &TempFile, write_fn: F) -> io::Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let file = tmp.file.try_clone()?;
    let mut buf = BufWriter::new(file);
    write_fn(&mut buf)?;
    buf.flush()?;
    let inner = buf.into_inner().map_err(|err| err.into_error())?;
    inner.sync_all()?;
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            return;
        }
        if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
            let _ = dir.sync_all();
        }
    }
}

/// Owns the temp file path + handle so callers can rely on RAII for
/// cleanup if they bail out mid-write.
struct TempFile {
    path: PathBuf,
    file: File,
}

impl TempFile {
    fn create(target: &Path) -> io::Result<Self> {
        let parent = target.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "atomic_io: destination '{}' has no parent directory",
                    target.display()
                ),
            )
        })?;
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
        let file_name = target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("file");
        let tmp_path = if parent.as_os_str().is_empty() {
            PathBuf::from(format!(".{file_name}.{}.tmp", uuid::Uuid::now_v7()))
        } else {
            parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::now_v7()))
        };
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        Ok(Self {
            path: tmp_path,
            file,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bytes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/state.json");
        atomic_write(&path, b"deep").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"deep");
    }

    #[test]
    fn streaming_writer_finalizes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        atomic_write_with(&path, |writer| {
            writeln!(writer, "first")?;
            writeln!(writer, "second")?;
            Ok(())
        })
        .unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read, "first\nsecond\n");
    }

    #[test]
    fn streaming_writer_cleans_up_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let err = atomic_write_with(&path, |_| Err(io::Error::other("nope"))).unwrap_err();
        assert_eq!(err.to_string(), "nope");
        assert!(!path.exists(), "destination should not exist after failure");
        // No leftover .tmp siblings.
        let leftover: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "tmp file should be cleaned up on error"
        );
    }

    #[test]
    fn concurrent_writers_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join("state.json"));
        let mut handles = Vec::new();
        for i in 0..16 {
            let path = std::sync::Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                let payload = format!("writer-{i}");
                atomic_write(&path, payload.as_bytes()).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        // The final contents must match exactly one of the writers — never a
        // truncated or interleaved value.
        let final_contents = std::fs::read_to_string(&*path).unwrap();
        assert!(
            final_contents.starts_with("writer-") && final_contents.len() <= "writer-15".len(),
            "unexpected final contents: {final_contents:?}"
        );
    }
}
