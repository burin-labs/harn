//! Copy-on-write filesystem overlay.
//!
//! Reads pass through to the real filesystem under [`OverlayFs::root`].
//! Writes (and deletes) land in an in-memory layer keyed by absolute
//! path, so a hermetic run can observe the underlying tree without ever
//! mutating it. Once the run finishes, [`OverlayFs::diff`] surfaces a
//! readable summary of every change — emit it as a unified diff, apply
//! it back with `git apply`, or discard it.
//!
//! Only the surface that stdlib `fs.*` builtins exercise is intercepted:
//! read/write text and bytes, append, exists, remove, copy, rename, list,
//! and create_dir. Metadata still falls through to the underlying fs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::testbench::tape::{self, TapeRecordKind};

/// One change in the overlay's write layer relative to the underlying
/// tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: PathBuf,
    pub kind: DiffKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// File created in the overlay (not in the underlying tree).
    Added { content: Vec<u8> },
    /// File present in the underlying tree, content changed in overlay.
    Modified { content: Vec<u8> },
    /// File present in the underlying tree, deleted in overlay.
    Deleted,
}

#[derive(Debug, Clone)]
enum OverlayEntry {
    File(Vec<u8>),
    Deleted,
    Directory,
}

#[derive(Debug)]
pub struct OverlayFs {
    root: PathBuf,
    layer: Mutex<BTreeMap<PathBuf, OverlayEntry>>,
}

impl OverlayFs {
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        // On macOS the kernel reports `getcwd` as the canonical
        // (`/private`-prefixed) path even when callers `set_current_dir`
        // to the un-prefixed form. Canonicalize the overlay root so
        // `within_root(...)` lines up with `resolve_source_relative_path`,
        // which sees post-canonicalization paths.
        let canonical = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        Self {
            root: normalize_logical(&canonical),
            layer: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn key(&self, path: &Path) -> PathBuf {
        canonicalize_for_overlay(path)
    }

    /// Whether `path` is inside the overlay's root. Calls outside the
    /// root fall through to the real filesystem so testbench-unaware
    /// helpers (the LLM provider's own caches, the runtime's session
    /// store) keep working.
    fn within_root(&self, path: &Path) -> bool {
        let key = self.key(path);
        key.starts_with(&self.root)
    }

    pub fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if !self.within_root(path) {
            return std::fs::read(path);
        }
        let key = self.key(path);
        let layer = self.layer.lock().expect("overlay layer poisoned");
        match layer.get(&key) {
            Some(OverlayEntry::File(bytes)) => Ok(bytes.clone()),
            Some(OverlayEntry::Deleted) => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("overlay: {} was deleted", key.display()),
            )),
            Some(OverlayEntry::Directory) => Err(std::io::Error::new(
                std::io::ErrorKind::IsADirectory,
                format!("overlay: {} is a directory", key.display()),
            )),
            None => std::fs::read(path),
        }
    }

    pub fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))
    }

    pub fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
        if !self.within_root(path) {
            return std::fs::write(path, contents);
        }
        let key = self.key(path);
        let mut layer = self.layer.lock().expect("overlay layer poisoned");
        layer.insert(key, OverlayEntry::File(contents.to_vec()));
        Ok(())
    }

    pub fn append(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
        if !self.within_root(path) {
            return std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| std::io::Write::write_all(&mut file, contents));
        }
        let mut combined = match self.read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err),
        };
        combined.extend_from_slice(contents);
        self.write(path, &combined)
    }

    pub fn copy(&self, src: &Path, dst: &Path) -> std::io::Result<u64> {
        let bytes = self.read(src)?;
        let len = bytes.len() as u64;
        self.write(dst, &bytes)?;
        Ok(len)
    }

    pub fn rename(&self, src: &Path, dst: &Path) -> std::io::Result<u64> {
        let len = self.copy(src, dst)?;
        self.remove_file(src)?;
        Ok(len)
    }

    pub fn exists(&self, path: &Path) -> bool {
        if !self.within_root(path) {
            return path.exists();
        }
        let key = self.key(path);
        let layer = self.layer.lock().expect("overlay layer poisoned");
        match layer.get(&key) {
            Some(OverlayEntry::File(_)) | Some(OverlayEntry::Directory) => true,
            Some(OverlayEntry::Deleted) => false,
            None => path.exists(),
        }
    }

    pub fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        if !self.within_root(path) {
            return std::fs::remove_file(path);
        }
        let key = self.key(path);
        let mut layer = self.layer.lock().expect("overlay layer poisoned");
        // Remove regardless of whether it exists in the underlying tree;
        // when the original is absent and the overlay had no entry, no-op.
        let underlying_present = path.exists();
        match layer.get(&key) {
            Some(OverlayEntry::Deleted) => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("overlay: {} already deleted", key.display()),
            )),
            _ => {
                layer.retain(|entry_path, _| !entry_path.starts_with(&key) || entry_path == &key);
                if underlying_present {
                    layer.insert(key, OverlayEntry::Deleted);
                } else {
                    layer.remove(&key);
                }
                Ok(())
            }
        }
    }

    pub fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        if !self.within_root(path) {
            return std::fs::create_dir_all(path);
        }
        let key = self.key(path);
        let mut layer = self.layer.lock().expect("overlay layer poisoned");
        if key == self.root {
            layer.insert(key, OverlayEntry::Directory);
            return Ok(());
        }
        let relative = key.strip_prefix(&self.root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "overlay: {} is outside {}",
                    key.display(),
                    self.root.display()
                ),
            )
        })?;
        let mut current = self.root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            layer.insert(current.clone(), OverlayEntry::Directory);
        }
        Ok(())
    }

    pub fn read_dir(&self, path: &Path) -> std::io::Result<Vec<OverlayDirEntry>> {
        if !self.within_root(path) {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                entries.push(OverlayDirEntry {
                    path: entry.path(),
                    is_dir: entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    is_file: entry.file_type().map(|t| t.is_file()).unwrap_or(false),
                });
            }
            return Ok(entries);
        }
        let dir_key = self.key(path);
        let virtual_dir_exists;
        {
            let layer = self.layer.lock().expect("overlay layer poisoned");
            match layer.get(&dir_key) {
                Some(OverlayEntry::Deleted) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("overlay: {} was deleted", dir_key.display()),
                    ));
                }
                Some(OverlayEntry::File(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        format!("overlay: {} is a file", dir_key.display()),
                    ));
                }
                Some(OverlayEntry::Directory) => {
                    virtual_dir_exists = true;
                }
                None => {
                    virtual_dir_exists = false;
                }
            }
        }
        let disk_dir_exists = path.exists();
        let mut entries: BTreeMap<PathBuf, OverlayDirEntry> = BTreeMap::new();
        if disk_dir_exists {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let p = entry.path();
                entries.insert(
                    p.clone(),
                    OverlayDirEntry {
                        path: p,
                        is_dir: entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                        is_file: entry.file_type().map(|t| t.is_file()).unwrap_or(false),
                    },
                );
            }
        }
        let layer = self.layer.lock().expect("overlay layer poisoned");
        for (key, entry) in layer.iter() {
            if key.parent() != Some(dir_key.as_path()) {
                continue;
            }
            match entry {
                OverlayEntry::File(_) => {
                    entries.insert(
                        key.clone(),
                        OverlayDirEntry {
                            path: key.clone(),
                            is_dir: false,
                            is_file: true,
                        },
                    );
                }
                OverlayEntry::Directory => {
                    entries.insert(
                        key.clone(),
                        OverlayDirEntry {
                            path: key.clone(),
                            is_dir: true,
                            is_file: false,
                        },
                    );
                }
                OverlayEntry::Deleted => {
                    entries.remove(key);
                }
            }
        }
        if entries.is_empty() && !disk_dir_exists && !virtual_dir_exists {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("overlay: {} was not found", dir_key.display()),
            ));
        }
        Ok(entries.into_values().collect())
    }

    /// Snapshot of every overlay change relative to the underlying tree.
    pub fn diff(&self) -> Vec<DiffEntry> {
        let layer = self.layer.lock().expect("overlay layer poisoned");
        let mut diff = Vec::new();
        for (path, entry) in layer.iter() {
            match entry {
                OverlayEntry::File(content) => {
                    if path.exists() {
                        let underlying = std::fs::read(path).unwrap_or_default();
                        if &underlying != content {
                            diff.push(DiffEntry {
                                path: path.clone(),
                                kind: DiffKind::Modified {
                                    content: content.clone(),
                                },
                            });
                        }
                    } else {
                        diff.push(DiffEntry {
                            path: path.clone(),
                            kind: DiffKind::Added {
                                content: content.clone(),
                            },
                        });
                    }
                }
                OverlayEntry::Deleted => {
                    if path.exists() {
                        diff.push(DiffEntry {
                            path: path.clone(),
                            kind: DiffKind::Deleted,
                        });
                    }
                }
                OverlayEntry::Directory => {}
            }
        }
        diff
    }

    /// Render the overlay's diff in unified-style format. Convenience
    /// wrapper around the standalone [`render_unified_diff`] that
    /// snapshots the layer first.
    pub fn render_unified_diff(&self) -> String {
        render_unified_diff(&self.diff())
    }
}

/// Render an overlay diff in unified-style format. Binary-safe but
/// non-text bytes are escaped via `String::from_utf8_lossy`, so this
/// is informational and not roundtrippable through `git apply` for
/// non-utf8 files.
pub fn render_unified_diff(diff: &[DiffEntry]) -> String {
    let mut out = String::new();
    for entry in diff {
        match &entry.kind {
            DiffKind::Added { content } => {
                out.push_str(&format!("--- /dev/null\n+++ b/{}\n", entry.path.display()));
                push_lines(&mut out, content, '+');
            }
            DiffKind::Modified { content } => {
                let underlying = std::fs::read(&entry.path).unwrap_or_default();
                out.push_str(&format!(
                    "--- a/{}\n+++ b/{}\n",
                    entry.path.display(),
                    entry.path.display()
                ));
                push_lines(&mut out, &underlying, '-');
                push_lines(&mut out, content, '+');
            }
            DiffKind::Deleted => {
                let underlying = std::fs::read(&entry.path).unwrap_or_default();
                out.push_str(&format!("--- a/{}\n+++ /dev/null\n", entry.path.display()));
                push_lines(&mut out, &underlying, '-');
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct OverlayDirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_file: bool,
}

fn push_lines(out: &mut String, bytes: &[u8], prefix: char) {
    let text = String::from_utf8_lossy(bytes);
    for line in text.split_inclusive('\n') {
        out.push(prefix);
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
}

/// Lexically normalize without resolving symlinks. Required because the
/// overlay layer is a logical map keyed by absolute path, not a real
/// filesystem; symlink chasing would be a security footgun.
fn normalize_logical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Make a path comparable to a canonicalized overlay root. If the file
/// itself canonicalizes (it exists on disk), use that. Otherwise
/// canonicalize the deepest existing ancestor and re-join the trailing
/// non-existent components, so a not-yet-written file under a real
/// directory still lands in the same key-space as the root.
fn canonicalize_for_overlay(path: &Path) -> PathBuf {
    let absolute = normalize_logical(path);
    if let Ok(direct) = std::fs::canonicalize(&absolute) {
        return direct;
    }
    let mut suffix = Vec::new();
    let mut probe = absolute.clone();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&probe) {
            let mut joined = canon;
            for component in suffix.iter().rev() {
                joined.push(component);
            }
            return joined;
        }
        match probe.file_name().map(|n| n.to_owned()) {
            Some(name) => {
                suffix.push(name);
                if !probe.pop() {
                    break;
                }
            }
            None => break,
        }
    }
    absolute
}

thread_local! {
    static ACTIVE_OVERLAY: RefCell<Option<Arc<OverlayFs>>> = const { RefCell::new(None) };
}

pub struct OverlayFsGuard {
    previous: Option<Arc<OverlayFs>>,
}

impl Drop for OverlayFsGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        ACTIVE_OVERLAY.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

pub fn install_overlay(overlay: Arc<OverlayFs>) -> OverlayFsGuard {
    let previous = ACTIVE_OVERLAY.with(|slot| slot.replace(Some(overlay)));
    OverlayFsGuard { previous }
}

pub fn active_overlay() -> Option<Arc<OverlayFs>> {
    ACTIVE_OVERLAY.with(|slot| slot.borrow().clone())
}

/// Helpers for fs builtins. Each helper falls through to `std::fs` when
/// no overlay is active, keeping the testbench opt-in.
///
/// Every successful read/write/delete also pushes a [`TapeRecordKind`]
/// into the active unified-tape recorder when one is installed, so the
/// fidelity oracle can compare FS effects across runs even when the
/// per-axis overlay diff is identical (the order in which writes land
/// also matters for replay determinism).
pub mod helpers {
    use super::*;

    fn record_file_read(path: &Path, bytes: &[u8]) {
        // Skip the hash + path stringification when no recorder is
        // installed — the fast path is the production path.
        if tape::active_recorder().is_none() {
            return;
        }
        let path_str = path.to_string_lossy().into_owned();
        let len = bytes.len() as u64;
        let hash = tape::content_hash(bytes);
        tape::with_active_recorder(|_recorder| {
            Some(TapeRecordKind::FileRead {
                path: path_str,
                content_hash: hash,
                len_bytes: len,
            })
        });
    }

    fn record_file_write(path: &Path, bytes: &[u8]) {
        if tape::active_recorder().is_none() {
            return;
        }
        let path_str = path.to_string_lossy().into_owned();
        let len = bytes.len() as u64;
        let hash = tape::content_hash(bytes);
        tape::with_active_recorder(|_recorder| {
            Some(TapeRecordKind::FileWrite {
                path: path_str,
                content_hash: hash,
                len_bytes: len,
            })
        });
    }

    fn record_file_delete(path: &Path) {
        if tape::active_recorder().is_none() {
            return;
        }
        let path_str = path.to_string_lossy().into_owned();
        tape::with_active_recorder(|_recorder| Some(TapeRecordKind::FileDelete { path: path_str }));
    }

    pub fn read(path: &Path) -> std::io::Result<Vec<u8>> {
        let result = match active_overlay() {
            Some(overlay) => overlay.read(path),
            None => std::fs::read(path),
        };
        if let Ok(bytes) = result.as_ref() {
            record_file_read(path, bytes);
        }
        result
    }

    pub fn read_to_string(path: &Path) -> std::io::Result<String> {
        let result = match active_overlay() {
            Some(overlay) => overlay.read_to_string(path),
            None => std::fs::read_to_string(path),
        };
        if let Ok(text) = result.as_ref() {
            record_file_read(path, text.as_bytes());
        }
        result
    }

    pub fn write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        let result = match active_overlay() {
            Some(overlay) => overlay.write(path, contents),
            None => atomic_write(path, contents),
        };
        if result.is_ok() {
            record_file_write(path, contents);
        }
        result
    }

    /// Crash-safe replacement for `std::fs::write`.
    ///
    /// `std::fs::write` opens the destination with `O_CREAT|O_TRUNC`, so it
    /// truncates an existing file to zero length *before* any byte is
    /// written. Any failure between that truncation and the completion of
    /// `write_all` (ENOSPC/EDQUOT, a failing/network fs returning EIO, or the
    /// process being killed mid-write) leaves the original content destroyed
    /// and unrecoverable, while the caller assumes the prior content survived.
    ///
    /// Instead we write the full contents into a sibling temp file, flush it,
    /// and atomically `rename` it over the destination. On POSIX `rename` is
    /// atomic and never leaves a half-written destination; if anything fails
    /// before the rename, the original file is untouched. The temp file is
    /// created in the destination's own directory so the rename stays within a
    /// single filesystem (a cross-device rename would fail with EXDEV).
    fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        use std::io::Write;

        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let dir = parent.unwrap_or_else(|| Path::new("."));

        // Unique, hidden sibling temp name. Including the pid and an atomic
        // counter keeps concurrent writers from colliding on the same temp
        // path.
        let counter = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            COUNTER.fetch_add(1, Ordering::Relaxed)
        };
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp_name = format!(".{file_name}.harn-tmp.{}.{counter}", std::process::id());
        let tmp_path = dir.join(tmp_name);

        // Write the full contents to the temp file, then fsync so the bytes
        // are durable before we swap it into place.
        let write_result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(contents)?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(err) = write_result {
            // Best-effort cleanup; the destination was never touched.
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }

        // Atomically replace the destination. On failure, clean up the temp
        // file and leave the original intact.
        if let Err(err) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }

    pub fn append(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        let result = match active_overlay() {
            Some(overlay) => overlay.append(path, contents),
            None => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| std::io::Write::write_all(&mut file, contents)),
        };
        if result.is_ok() {
            record_file_write(path, contents);
        }
        result
    }

    pub fn copy(src: &Path, dst: &Path) -> std::io::Result<u64> {
        match active_overlay() {
            Some(overlay) => {
                let result = overlay.copy(src, dst);
                if let Ok(bytes) = overlay.read(src) {
                    record_file_read(src, &bytes);
                    if result.is_ok() {
                        record_file_write(dst, &bytes);
                    }
                }
                result
            }
            None => {
                let copied = std::fs::copy(src, dst)?;
                if tape::active_recorder().is_some() {
                    let bytes = std::fs::read(dst)?;
                    record_file_read(src, &bytes);
                    record_file_write(dst, &bytes);
                }
                Ok(copied)
            }
        }
    }

    pub fn rename(src: &Path, dst: &Path) -> std::io::Result<u64> {
        match active_overlay() {
            Some(overlay) => {
                let bytes_for_record = overlay.read(src).ok();
                let result = overlay.rename(src, dst);
                if result.is_ok() {
                    if let Some(bytes) = bytes_for_record.as_deref() {
                        record_file_read(src, bytes);
                        record_file_write(dst, bytes);
                        record_file_delete(src);
                    }
                }
                result
            }
            None => {
                let bytes = tape::active_recorder()
                    .is_some()
                    .then(|| std::fs::read(src))
                    .transpose()?;
                let len = bytes
                    .as_ref()
                    .map(|bytes| bytes.len() as u64)
                    .or_else(|| std::fs::metadata(src).ok().map(|metadata| metadata.len()))
                    .unwrap_or(0);
                std::fs::rename(src, dst)?;
                if let Some(bytes) = bytes.as_deref() {
                    record_file_read(src, bytes);
                    record_file_write(dst, bytes);
                    record_file_delete(src);
                }
                Ok(len)
            }
        }
    }

    pub fn exists(path: &Path) -> bool {
        match active_overlay() {
            Some(overlay) => overlay.exists(path),
            None => path.exists(),
        }
    }

    pub fn remove_file(path: &Path) -> std::io::Result<()> {
        let result = match active_overlay() {
            Some(overlay) => overlay.remove_file(path),
            None => std::fs::remove_file(path),
        };
        if result.is_ok() {
            record_file_delete(path);
        }
        result
    }

    pub fn create_dir_all(path: &Path) -> std::io::Result<()> {
        match active_overlay() {
            Some(overlay) => overlay.create_dir_all(path),
            None => std::fs::create_dir_all(path),
        }
    }

    pub fn read_dir(path: &Path) -> std::io::Result<Vec<OverlayDirEntry>> {
        match active_overlay() {
            Some(overlay) => overlay.read_dir(path),
            None => {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(path)? {
                    let entry = entry?;
                    let file_type = entry.file_type()?;
                    entries.push(OverlayDirEntry {
                        path: entry.path(),
                        is_dir: file_type.is_dir(),
                        is_file: file_type.is_file(),
                    });
                }
                Ok(entries)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_land_in_overlay_only() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());
        overlay.write(&dir.path().join("hello.txt"), b"hi").unwrap();
        // Real disk untouched.
        assert!(!dir.path().join("hello.txt").exists());
        // Overlay reports it back.
        assert_eq!(
            overlay
                .read_to_string(&dir.path().join("hello.txt"))
                .unwrap(),
            "hi"
        );
    }

    #[test]
    fn reads_pass_through_to_underlying_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seed.txt"), "underlying").unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());
        assert_eq!(
            overlay
                .read_to_string(&dir.path().join("seed.txt"))
                .unwrap(),
            "underlying"
        );
    }

    #[test]
    fn delete_masks_underlying_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("doomed.txt"), "x").unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());
        overlay.remove_file(&dir.path().join("doomed.txt")).unwrap();
        assert!(!overlay.exists(&dir.path().join("doomed.txt")));
        // Real disk untouched.
        assert!(dir.path().join("doomed.txt").exists());
        let diff = overlay.diff();
        assert_eq!(diff.len(), 1);
        assert!(matches!(diff[0].kind, DiffKind::Deleted));
    }

    #[test]
    fn delete_masks_underlying_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("doomed");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("secret.txt"), "x").unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());

        overlay.remove_file(&nested).unwrap();

        assert!(!overlay.exists(&nested));
        assert_eq!(
            overlay.read_dir(&nested).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(nested.join("secret.txt").exists());
    }

    #[test]
    fn recursive_mkdir_creates_visible_overlay_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());
        overlay
            .create_dir_all(&dir.path().join("alpha/beta/gamma"))
            .unwrap();

        let root_entries = overlay.read_dir(&dir.path().join("alpha")).unwrap();
        assert_eq!(root_entries.len(), 1);
        assert_eq!(
            root_entries[0]
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("beta")
        );
        assert!(root_entries[0].is_dir);
    }

    #[test]
    fn read_dir_reports_missing_empty_overlay_path() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());

        assert_eq!(
            overlay
                .read_dir(&dir.path().join("missing"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }

    /// Regression: the live (no-overlay) write path must be crash-safe. A
    /// successful overwrite replaces the content and leaves no temp files.
    #[test]
    fn no_overlay_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("important.txt");
        std::fs::write(&target, "ORIGINAL IMPORTANT CONTENT").unwrap();
        assert!(active_overlay().is_none(), "no overlay should be installed");

        helpers::write(&target, b"NEW CONTENT").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW CONTENT");
        // No leftover temp files in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("harn-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// Regression for the non-atomic primary write path: a write that cannot
    /// be completed must leave the original file completely intact rather than
    /// truncating it.
    ///
    /// The trigger here is a read-only containing directory. The atomic path
    /// writes through a sibling temp file, so it cannot even start (temp
    /// `File::create` is denied) and the original survives untouched. The old
    /// `std::fs::write` path instead reopens the *existing* destination with
    /// `O_CREAT|O_TRUNC` — which needs no directory write permission — so it
    /// truncates and overwrites the original before any failure could protect
    /// it. The load-bearing assertion is therefore that the original content
    /// is preserved; under the buggy path it would read back as "NEW CONTENT".
    #[cfg(unix)]
    #[test]
    fn no_overlay_write_failure_preserves_original() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("important.txt");
        std::fs::write(&target, "ORIGINAL IMPORTANT CONTENT").unwrap();

        // Read+exec but not writable: a new sibling temp file cannot be
        // created, but the existing destination file is still openable.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        let result = helpers::write(&target, b"NEW CONTENT");

        // Restore write perms before asserting so tempdir drop/cleanup works.
        let mut restore = std::fs::metadata(dir.path()).unwrap().permissions();
        restore.set_mode(0o700);
        std::fs::set_permissions(dir.path(), restore).unwrap();

        // Load-bearing invariant: the original content must survive.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "ORIGINAL IMPORTANT CONTENT",
            "a write that cannot complete must not truncate or corrupt the original file"
        );
        // The atomic path also surfaces the failure rather than reporting a
        // false success.
        assert!(
            result.is_err(),
            "atomic write should report failure when it cannot create its temp file"
        );
    }

    #[test]
    fn diff_distinguishes_added_vs_modified() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "v1").unwrap();
        let overlay = OverlayFs::rooted_at(dir.path());
        overlay
            .write(&dir.path().join("existing.txt"), b"v2")
            .unwrap();
        overlay
            .write(&dir.path().join("brand-new.txt"), b"hi")
            .unwrap();
        let mut diff = overlay.diff();
        diff.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(diff.len(), 2);
        assert!(matches!(diff[0].kind, DiffKind::Added { .. }));
        assert!(matches!(diff[1].kind, DiffKind::Modified { .. }));
    }
}
