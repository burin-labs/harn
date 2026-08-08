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
//! 3. Flush userspace buffers and, when requested, `fsync` the temp file.
//! 4. Replace the destination atomically (`rename` on POSIX and
//!    `MoveFileExW(REPLACE_EXISTING)` on Windows).
//! 5. When requested, best-effort `fsync` the parent directory so the rename
//!    survives a power loss on filesystems that decouple the dirent from the
//!    inode.
//!
//! On any failure between (2) and (4), the temp file is removed so that
//! repeated retries don't leak `.tmp` siblings.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    static TEST_FAILURE_STAGE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fail_test_stage(stage: &'static str) -> io::Result<()> {
    if TEST_FAILURE_STAGE.with(|value| value.get()) == Some(stage) {
        return Err(io::Error::other(format!("injected {stage} failure")));
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn fail_test_stage(_stage: &'static str) -> io::Result<()> {
    Ok(())
}

/// Durability requested for an atomic namespace replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicWriteDurability {
    /// Readers never observe a partial payload. No storage flush is promised.
    Namespace,
    /// Flush the payload before replacement and request persistence of the
    /// namespace update. Filesystems and hardware may still have weaker
    /// guarantees than the operating-system call reports.
    Flush,
}

/// Storage-flush work completed by an atomic write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicWriteReceipt {
    /// The complete payload was flushed before replacement.
    pub file_synced: bool,
    /// Persistence of the namespace replacement was confirmed.
    pub namespace_synced: bool,
}

/// Atomically write `bytes` to `path`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |writer| writer.write_all(bytes))
}

/// Atomically write `bytes` to `path`, giving the file the Unix permission
/// bits `mode` (e.g. `0o600`).
///
/// The mode is applied to the temp file *before* the rename, so the bytes are
/// never observable at the process umask's default permissions — not even for
/// the width of the write. That ordering is the whole point of this variant:
/// writing first and `chmod`ing the destination afterwards leaves a window in
/// which a secret is world-readable. On non-Unix targets `mode` is ignored.
pub fn atomic_write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    atomic_write_stream_with_durability_and_mode(
        path,
        AtomicWriteDurability::Flush,
        Some(mode),
        |writer| writer.write_all(bytes),
    )
    .map(|_| ())
}

/// Atomically write `bytes` with an explicit durability request.
pub fn atomic_write_with_durability(
    path: &Path,
    bytes: &[u8],
    durability: AtomicWriteDurability,
) -> io::Result<AtomicWriteReceipt> {
    atomic_write_stream_with_durability_and_mode(path, durability, None, |writer| {
        writer.write_all(bytes)
    })
}

pub(crate) fn atomic_write_with_durability_unlocked(
    path: &Path,
    bytes: &[u8],
    durability: AtomicWriteDurability,
) -> io::Result<AtomicWriteReceipt> {
    atomic_write_stream_with_durability_and_mode_unlocked(path, durability, None, |writer| {
        writer.write_all(bytes)
    })
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
    atomic_write_stream_with_durability_and_mode(path, AtomicWriteDurability::Flush, None, write_fn)
        .map(|_| ())
}

fn atomic_write_stream_with_durability_and_mode<F>(
    path: &Path,
    durability: AtomicWriteDurability,
    mode: Option<u32>,
    write_fn: F,
) -> io::Result<AtomicWriteReceipt>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    // Windows refuses a replace while another writer is replacing the same
    // destination. Use the same canonical, cross-process lock as conditional
    // replacement instead of retrying on a timing-dependent access error.
    #[cfg(windows)]
    let _lock = crate::conditional_replace::acquire_lock(path)?;

    atomic_write_stream_with_durability_and_mode_unlocked(path, durability, mode, write_fn)
}

fn atomic_write_stream_with_durability_and_mode_unlocked<F>(
    path: &Path,
    durability: AtomicWriteDurability,
    mode: Option<u32>,
    write_fn: F,
) -> io::Result<AtomicWriteReceipt>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let mut tmp = TempFile::create(path, mode)?;
    let result = write_and_finalize(&mut tmp, durability, write_fn);
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp.path);
        return Err(err);
    }
    if let Err(err) = fail_test_stage("replace") {
        let _ = std::fs::remove_file(&tmp.path);
        return Err(err);
    }
    let replace_synced = match replace_temp_file(&tmp.path, path, durability) {
        Ok(synced) => synced,
        Err(err) => {
            let _ = std::fs::remove_file(&tmp.path);
            return Err(err);
        }
    };
    let namespace_synced = match durability {
        AtomicWriteDurability::Namespace => false,
        AtomicWriteDurability::Flush => replace_synced || sync_parent_dir(path),
    };
    Ok(AtomicWriteReceipt {
        file_synced: durability == AtomicWriteDurability::Flush,
        namespace_synced,
    })
}

fn write_and_finalize<F>(
    tmp: &mut TempFile,
    durability: AtomicWriteDurability,
    write_fn: F,
) -> io::Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let file = tmp
        .file
        .take()
        .ok_or_else(|| io::Error::other("atomic_io: temporary file handle was already consumed"))?;
    let mut buf = BufWriter::new(file);
    write_fn(&mut buf)?;
    fail_test_stage("flush")?;
    buf.flush()?;
    let inner = buf.into_inner().map_err(|err| err.into_error())?;
    if durability == AtomicWriteDurability::Flush {
        inner.sync_all()?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_temp_file(
    temp: &Path,
    destination: &Path,
    _durability: AtomicWriteDurability,
) -> io::Result<bool> {
    std::fs::rename(temp, destination)?;
    Ok(false)
}

#[cfg(windows)]
fn replace_temp_file(
    temp: &Path,
    destination: &Path,
    durability: AtomicWriteDurability,
) -> io::Result<bool> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temp_wide = crate::windows_path::wide_maybe_verbatim(temp);
    let destination_wide = crate::windows_path::wide_maybe_verbatim(destination);
    let mut flags = MOVEFILE_REPLACE_EXISTING;
    if durability == AtomicWriteDurability::Flush {
        flags |= MOVEFILE_WRITE_THROUGH;
    }

    // Unlike POSIX `rename`, which atomically replaces a destination even while
    // another process holds it open, `MoveFileExW` can transiently fail when a
    // virus scanner, the Windows indexer, or a lagging handle close briefly
    // holds the destination (ERROR_SHARING_VIOLATION) or its ACL check races
    // (ERROR_ACCESS_DENIED). Those windows are short-lived, so retry with a
    // small bounded backoff. This restores the rename tolerance the
    // pre-consolidation snapshot writer had, WITHOUT reintroducing its
    // destructive `remove_file(destination)` fallback (dropped deliberately so
    // a crash mid-replace can never leave the destination missing).
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const MAX_ATTEMPTS: u32 = 10;
    let mut backoff = std::time::Duration::from_millis(1);
    for attempt in 1..=MAX_ATTEMPTS {
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that remain
        // alive for the duration of the call.
        if unsafe { MoveFileExW(temp_wide.as_ptr(), destination_wide.as_ptr(), flags) } != 0 {
            return Ok(durability == AtomicWriteDurability::Flush);
        }
        let error = io::Error::last_os_error();
        let retryable = matches!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)
        );
        if !retryable || attempt == MAX_ATTEMPTS {
            return Err(error);
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    }
    unreachable!("the loop returns on the final attempt")
}

fn sync_parent_dir(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            return false;
        }
        if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
            return dir.sync_all().is_ok();
        }
    }
    false
}

/// Set `path`'s permission bits. Uses `set_permissions` rather than
/// `OpenOptions::mode` so the umask cannot widen or narrow the request.
#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Owns the temp file path + handle so callers can rely on RAII for
/// cleanup if they bail out mid-write.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
}

/// Longest prefix of the target file name kept in the temp sibling's name.
const TEMP_STEM_MAX: usize = 16;

/// Build the name of the temp file written next to `file_name` before the
/// atomic replace.
///
/// The temp sibling must not be meaningfully longer than the target it
/// replaces: embedding the full target name (which can be a 64-char content
/// hash) plus a hyphenated UUID made the temp path ~40 chars longer than the
/// target, so a target that fits under Windows' legacy 260-char `MAX_PATH`
/// could still produce a temp path that overflows it, failing `CreateFile`
/// with `ERROR_PATH_NOT_FOUND` (os error 3). A short recognizable prefix plus a
/// compact (unhyphenated) UUID keeps the temp co-located and unique while
/// bounding its length to a small constant regardless of the target name.
fn temp_sibling_name(file_name: &str) -> String {
    let stem: String = file_name.chars().take(TEMP_STEM_MAX).collect();
    format!(".{stem}.{}.tmp", uuid::Uuid::now_v7().simple())
}

impl TempFile {
    fn create(target: &Path, mode: Option<u32>) -> io::Result<Self> {
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
        let tmp_name = temp_sibling_name(file_name);
        let tmp_path = if parent.as_os_str().is_empty() {
            PathBuf::from(tmp_name)
        } else {
            parent.join(tmp_name)
        };
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        if let Some(mode) = mode {
            if let Err(error) = apply_mode(&tmp_path, mode) {
                drop(file);
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error);
            }
        } else if let Ok(metadata) = std::fs::metadata(target) {
            if let Err(error) = file.set_permissions(metadata.permissions()) {
                drop(file);
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error);
            }
        }
        Ok(Self {
            path: tmp_path,
            file: Some(file),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_sibling_name_is_length_bounded_regardless_of_target_name() {
        // A very long target name (e.g. a 64-char content hash, or longer) must
        // not inflate the temp sibling past a small constant, so a target that
        // fits under Windows' MAX_PATH can never produce an overflowing temp.
        let bound = 1 + TEMP_STEM_MAX + 1 + 32 + 4; // ".{<=16}.{32-hex uuid}.tmp"
        for name in ["s", "state.json", &"a".repeat(64), &"z".repeat(4096)] {
            let temp = temp_sibling_name(name);
            assert!(
                temp.len() <= bound,
                "temp name {:?} (len {}) exceeds bound {bound}",
                temp,
                temp.len()
            );
            assert!(temp.starts_with('.') && temp.ends_with(".tmp"));
        }
    }

    #[test]
    fn atomic_write_succeeds_for_a_long_target_file_name() {
        // The temp sibling used to embed the full (long) target name, so this
        // write overflowed MAX_PATH on Windows. It must succeed on every OS.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a".repeat(200));
        atomic_write(&path, b"payload").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"payload");
    }

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
        std::fs::write(&path, b"old").unwrap();
        let err = atomic_write_with(&path, |writer| {
            writer.write_all(b"partial")?;
            Err(io::Error::other("nope"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "nope");
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
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
    fn flush_and_replace_failures_preserve_destination_and_clean_up() {
        for stage in ["flush", "replace"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            std::fs::write(&path, b"old").unwrap();
            TEST_FAILURE_STAGE.with(|value| value.set(Some(stage)));
            let error = atomic_write(&path, b"new").unwrap_err();
            TEST_FAILURE_STAGE.with(|value| value.set(None));

            assert_eq!(error.to_string(), format!("injected {stage} failure"));
            assert_eq!(std::fs::read(&path).unwrap(), b"old");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .collect();
            assert!(leftovers.is_empty(), "{stage} left a temp file");
        }
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn mode_is_applied_before_the_rename() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        atomic_write_with_mode(&path, b"secret", 0o600).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "credentials must be owner-only");
    }

    #[cfg(unix)]
    #[test]
    fn mode_survives_overwriting_a_loose_destination() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_with_mode(&path, b"secret", 0o600).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
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
