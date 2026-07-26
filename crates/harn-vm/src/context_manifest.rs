//! Stat-based validity proof for an entry chunk's import-graph context.
//!
//! The entry-chunk cache key folds in the content of every transitively
//! reachable user file, so deciding whether a cached chunk is still valid used
//! to mean re-reading, re-scanning and re-hashing that whole graph on every
//! spawn — a cold-path algorithm running on the warm path.
//!
//! A manifest records what the graph looked like when the key was computed, in
//! terms cheap enough to re-check: each file's stat identity, and the negative
//! facts the graph also depends on. Re-checking is stats only. Any mismatch,
//! any file that cannot be stat'ed, and any manifest that was never written
//! falls back to the full walk, which recomputes the key from scratch — so a
//! manifest can only ever save work, never decide a hit on its own.
//!
//! Stat identity is already the trust boundary inside a process:
//! [`crate::module_source`] memoizes reads on `(path, len, mtime_ns)`. This
//! extends that same decision across process boundaries, matching how Cargo,
//! Zig and Bazel gate their warm paths. The accepted gap is identical to
//! theirs: an edit that preserves both length and mtime is not noticed. That
//! gap is pinned by a test rather than left to prose, so closing it — or
//! widening it — has to be a deliberate edit and cannot happen by accident.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::module_source;

/// One transitively reachable source file, plus the path that must stay absent
/// for the imports that reached it to keep resolving here.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestFile {
    /// Canonical path, matching the key the walk dedups on.
    pub path: PathBuf,
    pub len: u64,
    pub mtime_ns: i128,
    /// Extensionless sibling that would shadow this file if it appeared.
    ///
    /// `resolve_local_import` probes `base.join(import)` *before* appending
    /// `.harn`, so creating `dep/` next to `dep.harn` silently re-points every
    /// `import "./dep"` at the new directory. Refactoring a module into a
    /// directory is an ordinary thing to do, and without this the cache would
    /// keep serving bytecode compiled against the file it replaced.
    pub shadow: Option<PathBuf>,
}

/// An import that resolved to nothing when the key was computed.
///
/// The graph depends on this staying true: a file that appears later adds a
/// real dependency without changing any recorded file's content.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestUnresolved {
    pub anchor: PathBuf,
    pub import: String,
}

/// A path an import resolved to that could not be read.
///
/// Real trees contain these: an `import "./types"` where `types/` is a
/// directory resolves, then fails to read. The error *kind* is folded into the
/// key, so the manifest has to reproduce it exactly rather than approximate it
/// from a stat — which is why this re-attempts the read. There are only ever a
/// handful of these, so re-reading them is cheaper than the walk they avoid.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestUnreadable {
    pub path: PathBuf,
    pub kind: String,
}

/// Everything the entry key's import-graph walk observed, in re-checkable form.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextManifest {
    pub files: Vec<ManifestFile>,
    pub unresolved: Vec<ManifestUnresolved>,
    pub unreadable: Vec<ManifestUnreadable>,
}

impl ContextManifest {
    /// Whether the graph still looks exactly as it did when this manifest was
    /// written.
    ///
    /// Conservative in every direction: anything unreadable, ambiguous, or
    /// changed reports `false` and costs a walk.
    pub fn still_valid(&self) -> bool {
        self.files.iter().all(ManifestFile::still_valid)
            && self
                .unresolved
                .iter()
                .all(ManifestUnresolved::still_unresolved)
            && self
                .unreadable
                .iter()
                .all(ManifestUnreadable::still_unreadable)
    }
}

impl ManifestFile {
    /// Record `path` as observed on disk now, or `None` if it cannot be stat'ed
    /// — a file we cannot describe is one we must not claim is unchanged.
    pub fn observe(path: &Path) -> Option<Self> {
        let (len, mtime_ns) = module_source::stat_identity(path)?;
        Some(Self {
            path: path.to_path_buf(),
            len,
            mtime_ns,
            shadow: shadow_path(path),
        })
    }

    pub(crate) fn still_valid(&self) -> bool {
        let Some((len, mtime_ns)) = module_source::stat_identity(&self.path) else {
            return false;
        };
        if len != self.len || mtime_ns != self.mtime_ns {
            return false;
        }
        !self.shadow.as_ref().is_some_and(|shadow| shadow.exists())
    }
}

impl ManifestUnreadable {
    pub(crate) fn still_unreadable(&self) -> bool {
        match module_source::read(&self.path) {
            Ok(_) => false,
            Err(error) => error.kind().to_string() == self.kind,
        }
    }
}

impl ManifestUnresolved {
    pub(crate) fn still_unresolved(&self) -> bool {
        harn_modules::resolve_import_path(&self.anchor, &self.import).is_none()
    }
}

/// The extensionless path that would shadow `path`, for `*.harn` files only.
fn shadow_path(path: &Path) -> Option<PathBuf> {
    if path.extension()? != "harn" {
        return None;
    }
    let mut shadow = path.to_path_buf();
    shadow.set_extension("");
    Some(shadow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn manifest_for(paths: &[PathBuf]) -> ContextManifest {
        ContextManifest {
            files: paths
                .iter()
                .map(|p| ManifestFile::observe(p).expect("observe"))
                .collect(),
            unresolved: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// State a file's mtime outright, so a test can say which version it means
    /// instead of hoping the clock moved between two writes.
    ///
    /// Two writes in quick succession routinely share an mtime: Windows' system
    /// clock ticks at ~15.6ms, and HFS+/ext3/some NFS store whole seconds. A
    /// test that rewrites a file and expects the stat to differ is asserting
    /// something about the host's timer, not about this module.
    /// `module_source`'s twin test (`a_same_length_edit_is_re_read_in_a_warm_process`)
    /// sets the timestamp for exactly this reason rather than sleeping out the
    /// coarsest plausible tick.
    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    /// Move `path`'s mtime `secs` forward, making the current contents
    /// unambiguously newer than anything observed before now.
    fn advance_mtime(path: &Path, secs: u64) {
        let current = std::fs::metadata(path).unwrap().modified().unwrap();
        set_mtime(path, current + std::time::Duration::from_secs(secs));
    }

    #[test]
    fn an_unchanged_graph_stays_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        assert!(manifest_for(&[dep]).still_valid());
    }

    #[test]
    fn a_same_length_edit_invalidates() {
        // Length alone cannot carry the check: `mtime_ns` is what catches an
        // edit that keeps the file the same size. The in-process read memo
        // relies on exactly this, and so does the manifest.
        //
        // The rewrite's mtime is set rather than left to the clock. Without
        // that this asserts the host produced two distinguishable timestamps
        // within a few microseconds, which Windows does not
        // (see `set_mtime`), so the test failed there while passing
        // everywhere else — a portability bug in the test, not a change in
        // what this module guarantees. The guarantee under test is that a
        // *distinguishable* mtime is noticed; the case where it is not
        // distinguishable is pinned by the test below.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 111 }\n");
        let manifest = manifest_for(&[dep.clone()]);
        write(&dep, "pub fn v() -> int { return 222 }\n");
        advance_mtime(&dep, 10);
        assert_eq!(
            std::fs::metadata(&dep).unwrap().len(),
            33,
            "both versions must be the same byte length or this exercises \
             the length path instead of the mtime path"
        );
        assert!(
            !manifest.still_valid(),
            "an edit of identical length must still invalidate the manifest"
        );
    }

    #[test]
    fn a_same_length_edit_under_one_mtime_tick_is_the_documented_gap() {
        // The stated limit of a stat-only proof, made executable so it is a
        // decision rather than a footnote: an edit that preserves BOTH length
        // and mtime is not noticed. Reproduced deterministically by restoring
        // the original timestamp, because the filesystems where this happens
        // for real (Windows' ~15.6ms clock tick, 1s-granularity filesystems)
        // are not the ones CI necessarily runs on.
        //
        // This is inherited, not introduced by the manifest: `module_source`'s
        // in-process memo keys on the same `(len, mtime_ns)` identity and has
        // the same blind spot. If that identity is ever strengthened, this
        // test should fail and be deleted deliberately — it exists so the
        // trade-off cannot be changed by accident in either direction.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 111 }\n");
        let original = std::fs::metadata(&dep).unwrap().modified().unwrap();
        let manifest = manifest_for(&[dep.clone()]);

        write(&dep, "pub fn v() -> int { return 222 }\n");
        set_mtime(&dep, original);

        assert!(
            manifest.still_valid(),
            "a stat-only manifest cannot notice an edit that preserves both \
             length and mtime; if this now fails, the identity was \
             strengthened and this test should be removed on purpose"
        );
    }

    #[test]
    fn a_deleted_file_invalidates() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let manifest = manifest_for(&[dep.clone()]);
        std::fs::remove_file(&dep).unwrap();
        assert!(!manifest.still_valid());
    }

    #[test]
    fn a_directory_that_would_shadow_the_module_invalidates() {
        // Refactoring `dep.harn` into `dep/` re-points every `import "./dep"`
        // without touching dep.harn, because the resolver probes the
        // extensionless path first. Nothing about the recorded file changes,
        // so only the shadow check can catch it.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let manifest = manifest_for(&[dep]);
        assert!(manifest.still_valid());

        std::fs::create_dir(tmp.path().join("dep")).unwrap();
        assert!(
            !manifest.still_valid(),
            "a directory shadowing the module file must invalidate the manifest"
        );
    }

    #[test]
    fn an_import_that_starts_resolving_invalidates() {
        // The mirror of the file checks: no recorded file changes at all, but
        // the graph gains a dependency it did not have.
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp.path().join("entry.harn");
        write(&entry, "import \"./late\"\n");
        let manifest = ContextManifest {
            files: Vec::new(),
            unresolved: vec![ManifestUnresolved {
                anchor: entry,
                import: "./late".to_string(),
            }],
            unreadable: Vec::new(),
        };
        assert!(manifest.still_valid());

        write(
            &tmp.path().join("late.harn"),
            "pub fn l() -> int { return 1 }\n",
        );
        assert!(
            !manifest.still_valid(),
            "an import that now resolves must invalidate the manifest"
        );
    }

    #[test]
    fn a_file_without_a_harn_extension_has_no_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let odd = tmp.path().join("dep");
        write(&odd, "pub fn v() -> int { return 1 }\n");
        assert_eq!(ManifestFile::observe(&odd).unwrap().shadow, None);
    }
}
