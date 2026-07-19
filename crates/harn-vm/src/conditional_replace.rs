//! Cross-process compare-and-replace for complete file payloads.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::atomic_io::{
    atomic_write_with_durability_unlocked, AtomicWriteDurability, AtomicWriteReceipt,
};

thread_local! {
    static EXECUTION_LOCK_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Restores the prior execution-local replacement lock root on drop.
#[derive(Debug)]
#[must_use = "retain this guard for the execution that owns the lock root"]
pub struct ScopedConditionalReplaceLockRoot {
    previous: Option<PathBuf>,
}

/// Route compare-and-replace locks through a caller-owned root on this thread.
/// Worker threads must install their own guard; the override is intended for
/// isolated embedders and tests, not as ambient workflow state.
pub fn scope_conditional_replace_lock_root(
    root: impl AsRef<Path>,
) -> ScopedConditionalReplaceLockRoot {
    let previous = EXECUTION_LOCK_ROOT.with(|slot| slot.replace(Some(root.as_ref().to_path_buf())));
    ScopedConditionalReplaceLockRoot { previous }
}

impl Drop for ScopedConditionalReplaceLockRoot {
    fn drop(&mut self) {
        EXECUTION_LOCK_ROOT.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

/// Preconditions and durability for one replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalReplaceOptions {
    /// Digest observed by the caller, including the `sha256:` prefix.
    pub expected_sha256: Option<String>,
    /// Allow a missing destination to be created.
    pub create: bool,
    /// Allow an existing destination to change.
    pub overwrite: bool,
    /// Create a missing parent chain.
    pub create_parents: bool,
    /// Requested namespace or storage-flush durability.
    pub durability: AtomicWriteDurability,
}

impl Default for ConditionalReplaceOptions {
    fn default() -> Self {
        Self {
            expected_sha256: None,
            create: true,
            overwrite: true,
            create_parents: true,
            durability: AtomicWriteDurability::Flush,
        }
    }
}

/// Closed successful outcome for one replacement attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionalReplaceStatus {
    Created,
    Replaced,
    NoOp,
    Stale,
}

impl ConditionalReplaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Replaced => "replaced",
            Self::NoOp => "no_op",
            Self::Stale => "stale",
        }
    }
}

/// Receipt for one compare-and-replace operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalReplaceReceipt {
    /// Closed operation outcome.
    pub status: ConditionalReplaceStatus,
    /// Whether the destination existed before this attempt.
    pub before_exists: bool,
    /// Digest observed while the replacement lock was held.
    pub before_sha256: String,
    /// Digest of the requested complete payload.
    pub after_sha256: String,
    /// Caller-supplied lease, when present.
    pub expected_sha256: Option<String>,
    /// Payload bytes written; zero for `NoOp` and `Stale`.
    pub bytes_written: usize,
    /// Whether the complete payload was flushed to the operating system.
    pub file_synced: bool,
    /// Whether persistence of the namespace replacement was confirmed.
    pub namespace_synced: bool,
}

/// Replace a complete file payload under a canonical-path cross-process lock.
pub fn conditional_replace(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
) -> io::Result<ConditionalReplaceReceipt> {
    conditional_replace_with_hook(path, contents, options, || {})
}

/// Variant used by hosts that must snapshot the pre-image immediately before
/// mutation while the replacement lock is still held.
pub fn conditional_replace_with_hook<F>(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
    before_write: F,
) -> io::Result<ConditionalReplaceReceipt>
where
    F: FnOnce(),
{
    conditional_replace_with_io(
        path,
        contents,
        options,
        |candidate| {
            reject_symlink_destination(candidate)?;
            fs::read(candidate)
        },
        |candidate, bytes, durability, create_parents| {
            require_parent(candidate, create_parents)?;
            // The compare-and-replace lock already covers the complete read,
            // lease check, and write boundary.
            atomic_write_with_durability_unlocked(candidate, bytes, durability)
        },
        before_write,
    )
}

pub(crate) fn conditional_replace_with_io<R, W, F>(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
    read: R,
    write: W,
    before_write: F,
) -> io::Result<ConditionalReplaceReceipt>
where
    R: FnOnce(&Path) -> io::Result<Vec<u8>>,
    W: FnOnce(&Path, &[u8], AtomicWriteDurability, bool) -> io::Result<AtomicWriteReceipt>,
    F: FnOnce(),
{
    let _lock = acquire_lock(path)?;
    let (before, before_exists) = match read(path) {
        Ok(bytes) => (bytes, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), false),
        Err(error) => return Err(error),
    };
    let before_sha256 = sha256_label(&before);
    let after_sha256 = sha256_label(contents);

    if options
        .expected_sha256
        .as_deref()
        .is_some_and(|expected| expected != before_sha256)
    {
        return Ok(ConditionalReplaceReceipt {
            status: ConditionalReplaceStatus::Stale,
            before_exists,
            before_sha256,
            after_sha256,
            expected_sha256: options.expected_sha256.clone(),
            bytes_written: 0,
            file_synced: false,
            namespace_synced: false,
        });
    }

    if before_exists && before == contents {
        return Ok(ConditionalReplaceReceipt {
            status: ConditionalReplaceStatus::NoOp,
            before_exists: true,
            before_sha256,
            after_sha256,
            expected_sha256: options.expected_sha256.clone(),
            bytes_written: 0,
            file_synced: false,
            namespace_synced: false,
        });
    }
    if before_exists && !options.overwrite {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("'{}' exists and overwrite=false", path.display()),
        ));
    }
    if !before_exists && !options.create {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("'{}' does not exist and create=false", path.display()),
        ));
    }
    before_write();
    let durability = write(path, contents, options.durability, options.create_parents)?;
    Ok(ConditionalReplaceReceipt {
        status: if before_exists {
            ConditionalReplaceStatus::Replaced
        } else {
            ConditionalReplaceStatus::Created
        },
        before_exists,
        before_sha256,
        after_sha256,
        expected_sha256: options.expected_sha256.clone(),
        bytes_written: contents.len(),
        file_synced: durability.file_synced,
        namespace_synced: durability.namespace_synced,
    })
}

fn reject_symlink_destination(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to replace symlink destination '{}'",
                path.display()
            ),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn require_parent(path: &Path, create_parents: bool) -> io::Result<()> {
    if create_parents {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "parent directory for '{}' does not exist (pass create_parents=true to create it)",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn sha256_label(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn lock_root() -> PathBuf {
    if let Some(root) = EXECUTION_LOCK_ROOT.with(|slot| slot.borrow().clone()) {
        return root;
    }
    let runtime_root = crate::stdlib::process::runtime_root_base();
    crate::runtime_paths::state_root(&runtime_root).join("fs-cas-locks")
}

pub(crate) fn acquire_lock(path: &Path) -> io::Result<ConditionalReplaceLock> {
    let root = lock_root();
    fs::create_dir_all(&root)?;
    let identity = canonical_lock_identity(path);
    let name = format!(
        "{}.lock",
        hex::encode(Sha256::digest(lock_identity_bytes(&identity)))
    );
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(name))?;
    file.lock_exclusive()?;
    Ok(ConditionalReplaceLock { file })
}

fn lock_identity_bytes(identity: &Path) -> Vec<u8> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        identity.to_string_lossy().to_lowercase().into_bytes()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        identity.as_os_str().as_encoded_bytes().to_vec()
    }
}

fn canonical_lock_identity(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while let Some(name) = ancestor.file_name() {
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if let Ok(canonical_parent) = fs::canonicalize(parent) {
            let mut identity = canonical_parent;
            for component in suffix.iter().rev() {
                identity.push(component);
            }
            return identity;
        }
        ancestor = parent;
    }
    absolute
}

pub(crate) struct ConditionalReplaceLock {
    file: File,
}

impl Drop for ConditionalReplaceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn stale_digest_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
        let path = dir.path().join("state.json");
        fs::write(&path, b"current").unwrap();
        let options = ConditionalReplaceOptions {
            expected_sha256: Some(sha256_label(b"older")),
            ..ConditionalReplaceOptions::default()
        };
        let receipt = conditional_replace(&path, b"new", &options).unwrap();
        assert_eq!(receipt.status, ConditionalReplaceStatus::Stale);
        assert_eq!(fs::read(&path).unwrap(), b"current");
    }

    #[test]
    fn create_replace_and_no_op_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
        let path = dir.path().join("state.json");
        let options = ConditionalReplaceOptions::default();

        let created = conditional_replace(&path, b"one", &options).unwrap();
        assert_eq!(created.status, ConditionalReplaceStatus::Created);
        let no_op = conditional_replace(&path, b"one", &options).unwrap();
        assert_eq!(no_op.status, ConditionalReplaceStatus::NoOp);
        let replaced = conditional_replace(&path, b"two", &options).unwrap();
        assert_eq!(replaced.status, ConditionalReplaceStatus::Replaced);
    }

    #[test]
    fn one_concurrent_writer_wins_an_observed_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("state.json"));
        let lock_root = Arc::new(dir.path().join("locks"));
        fs::write(path.as_ref(), b"original").unwrap();
        let expected = sha256_label(b"original");
        let barrier = Arc::new(Barrier::new(17));
        let mut workers = Vec::new();
        for index in 0..16 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            let expected = expected.clone();
            let lock_root = Arc::clone(&lock_root);
            workers.push(std::thread::spawn(move || {
                let _locks = scope_conditional_replace_lock_root(lock_root.as_ref());
                let payload = format!("writer-{index}");
                let options = ConditionalReplaceOptions {
                    expected_sha256: Some(expected),
                    ..ConditionalReplaceOptions::default()
                };
                barrier.wait();
                conditional_replace(path.as_ref(), payload.as_bytes(), &options).unwrap()
            }));
        }
        barrier.wait();
        let receipts: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| receipt.status == ConditionalReplaceStatus::Replaced)
                .count(),
            1
        );
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| receipt.status == ConditionalReplaceStatus::Stale)
                .count(),
            15
        );
    }

    #[test]
    fn canonical_path_aliases_share_a_lock_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let path = dir.path().join("state.json");
        let alias = dir.path().join("sub/../state.json");
        fs::write(&path, b"original").unwrap();
        assert_eq!(
            canonical_lock_identity(&path),
            canonical_lock_identity(&alias)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_destinations_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
        let target = dir.path().join("target.txt");
        let alias = dir.path().join("alias.txt");
        fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();

        let error =
            conditional_replace(&alias, b"new", &ConditionalReplaceOptions::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert!(fs::symlink_metadata(alias)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn write_failure_preserves_the_preimage() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
        let path = dir.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let hook_calls = std::cell::Cell::new(0);
        let error = conditional_replace_with_io(
            &path,
            b"new",
            &ConditionalReplaceOptions::default(),
            |candidate| fs::read(candidate),
            |_, _, _, _| Err(io::Error::other("injected write failure")),
            || hook_calls.set(hook_calls.get() + 1),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "injected write failure");
        assert_eq!(hook_calls.get(), 1);
        assert_eq!(fs::read(path).unwrap(), b"old");
    }

    #[test]
    fn create_and_overwrite_policies_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
        let path = dir.path().join("state.json");
        let no_create = ConditionalReplaceOptions {
            create: false,
            ..ConditionalReplaceOptions::default()
        };
        assert_eq!(
            conditional_replace(&path, b"new", &no_create)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        fs::write(&path, b"old").unwrap();
        let no_overwrite = ConditionalReplaceOptions {
            overwrite: false,
            ..ConditionalReplaceOptions::default()
        };
        assert_eq!(
            conditional_replace(&path, b"new", &no_overwrite)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(path).unwrap(), b"old");
    }
}
