//! Root-anchored restore effects. On Unix the guard check, write, and delete run
//! through directory descriptors opened with no-follow flags (see [`fd`]);
//! other targets resolve paths by name and check ancestors first.

#[cfg(not(unix))]
use std::fs as stdfs;
use std::path::Path;
#[cfg(not(unix))]
use std::path::PathBuf;

#[cfg(unix)]
mod fd;

#[cfg(not(unix))]
use super::{atomic_write, current_hash};
use super::{PlanEntry, WritePayload};

/// Outcome of one guarded restore step.
pub(super) enum Step {
    Restored,
    /// The guard hash no longer matched; carries the hash found.
    Refused(Option<String>),
}

#[cfg(all(test, unix))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stage {
    /// Planning is done; nothing about the path has been opened yet.
    BeforeWalk,
    /// The parent directory is resolved; the write or delete is next.
    AfterWalk,
    /// The last content guard passed, immediately before the replacement.
    AfterFinalGuard,
}

#[cfg(all(test, unix))]
type RestoreHook = Box<dyn Fn(Stage)>;

#[cfg(all(test, unix))]
thread_local! {
    pub(super) static RESTORE_HOOK: std::cell::RefCell<Option<RestoreHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
pub(super) fn restore_hook(stage: Stage) {
    RESTORE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(stage);
        }
    });
}

#[cfg(unix)]
pub(super) type Anchor = fd::Root;

/// Opened once per restore. Non-Unix targets have no `openat`, so they keep the
/// name-based path with the ancestor check.
#[cfg(not(unix))]
pub(super) struct Anchor(PathBuf);

#[cfg(unix)]
pub(super) fn open_anchor(root: &Path) -> Result<Anchor, String> {
    fd::Root::open(root)
}

#[cfg(not(unix))]
pub(super) fn open_anchor(root: &Path) -> Result<Anchor, String> {
    Ok(Anchor(root.to_path_buf()))
}

/// Check the guard, then write or delete, with every step relative to the
/// directory descriptors opened from the root. A symlink or non-directory
/// component is an error, never followed.
#[cfg(unix)]
pub(super) fn restore_one(
    anchor: &Anchor,
    entry: &PlanEntry,
    guard: &Option<String>,
    write: Option<&WritePayload>,
    require_no_replace: bool,
) -> Result<Step, String> {
    #[cfg(all(test, unix))]
    restore_hook(Stage::BeforeWalk);
    let parent = anchor.parent_of(&entry.path, write.is_some())?;
    #[cfg(all(test, unix))]
    restore_hook(Stage::AfterWalk);
    let Some(parent) = parent else {
        // A missing directory means a missing file; only a recorded deletion matches.
        return match guard {
            None if write.is_none() => Ok(Step::Restored),
            None => Err(format!("{}: parent directory vanished", entry.path)),
            Some(_) => Ok(Step::Refused(None)),
        };
    };
    let found = parent.hash()?;
    if found != *guard {
        return Ok(Step::Refused(found));
    }
    if require_no_replace && (guard.is_some() || write.is_none()) {
        return Ok(Step::Refused(found));
    }
    match write {
        Some(payload) => {
            // Re-verify through the same descriptor before publishing.
            let written =
                parent.write(&payload.bytes, payload.exec, require_no_replace, |dir| {
                    if dir.hash()? == *guard {
                        #[cfg(all(test, unix))]
                        restore_hook(Stage::AfterFinalGuard);
                        Ok(())
                    } else {
                        Err(format!("{} changed while restoring", entry.path))
                    }
                })?;
            if !written {
                return Ok(Step::Refused(parent.hash()?));
            }
        }
        None => parent.delete()?,
    }
    Ok(Step::Restored)
}

#[cfg(not(unix))]
pub(super) fn restore_one(
    anchor: &Anchor,
    entry: &PlanEntry,
    guard: &Option<String>,
    write: Option<&WritePayload>,
    require_no_replace: bool,
) -> Result<Step, String> {
    if require_no_replace {
        return Err("concurrent-safe restore is unavailable on this platform".to_string());
    }
    let root = &anchor.0;
    let full = root.join(&entry.path);
    let found = current_hash(&full)?;
    if found != *guard {
        return Ok(Step::Refused(found));
    }
    ensure_plain_ancestors(root, &full)?;
    match write {
        Some(payload) => {
            if let Some(parent) = full.parent() {
                stdfs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            atomic_write(&full, &payload.bytes)?;
            set_executable(&full, payload.exec);
        }
        None => {
            stdfs::remove_file(&full).map_err(|e| format!("delete {}: {e}", full.display()))?;
            let mut dir = full.parent();
            while let Some(parent) = dir.filter(|d| *d != root && d.starts_with(root)) {
                if stdfs::remove_dir(parent).is_err() {
                    break;
                }
                dir = parent.parent();
            }
        }
    }
    Ok(Step::Restored)
}

/// Refuse a path whose ancestors below `root` include a symlink or a non-directory.
#[cfg(not(unix))]
fn ensure_plain_ancestors(root: &Path, full: &Path) -> Result<(), String> {
    let mut dir = full.parent();
    let mut chain = Vec::new();
    while let Some(d) = dir.filter(|d| *d != root && d.starts_with(root)) {
        chain.push(d);
        dir = d.parent();
    }
    for d in chain.into_iter().rev() {
        match stdfs::symlink_metadata(d) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is a symlink or not a directory; refusing to write through it",
                    d.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("stat {}: {e}", d.display())),
        }
    }
    Ok(())
}

/// Windows has no execute bit; the flag is accepted so restore reads it everywhere.
#[cfg(not(unix))]
fn set_executable(_path: &Path, _exec: bool) {}
