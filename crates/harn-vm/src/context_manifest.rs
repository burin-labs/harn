//! Stat-based validity proof for an entry chunk's import-graph context.
//!
//! The entry-chunk cache key folds in the content of every transitively
//! reachable user file, so deciding whether a cached chunk is still valid used
//! to mean re-reading, re-scanning and re-hashing that whole graph on every
//! spawn — a cold-path algorithm running on the warm path.
//!
//! A manifest records what the graph looked like when the key was computed, in
//! terms cheap enough to re-check: the entry it was walked from, each file's
//! stat identity, and the negative facts the graph also depends on. The anchor
//! is what makes the rest mean anything — the same set of unchanged files
//! describes a different graph under a different entry, and a cache that names
//! artifacts by entry source hash alone will hand one entry the other's
//! manifest.
//!
//! Re-checking is stats only. A different anchor, any mismatch, any file that
//! cannot be stat'ed, and any manifest that was never written all fall back to
//! the full walk, which recomputes the key from scratch — so a manifest can
//! only ever save work, never decide a hit on its own.
//!
//! Stat identity is already the trust boundary inside a process:
//! [`crate::module_source`] memoizes reads on `(path, len, mtime_ns)`. This
//! extends that same decision across process boundaries, matching how Cargo,
//! Zig and Bazel gate their warm paths.
//!
//! The gap in a stat-only proof is an edit that preserves both length and
//! mtime. Most of that gap is not adversarial but mechanical: on a filesystem
//! storing mtime to the second, a write landing in the same tick as the walk
//! leaves the recorded identity correct and already stale, which is exactly
//! what fast automated edits produce. So the manifest records when its walk
//! began and declines to vouch for any file whose mtime is not safely older
//! than that — the racily-clean rule Git, make and ccache all apply. The cost
//! lands only on files written moments before the walk, and declining costs
//! one full walk that recomputes the key anyway.
//!
//! What remains is the deliberate case: a file rewritten long afterwards with
//! its old length and its old mtime restored. Both halves of that boundary are
//! pinned by tests, so narrowing it further — or widening it — has to be a
//! deliberate edit and cannot happen by accident.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::module_source;

/// Wall-clock nanoseconds, on the same epoch as the mtimes it is compared with.
///
/// Real wall time is inherent here rather than incidental: the value only has
/// meaning next to filesystem mtimes, which no in-process clock controls.
fn now_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        // Before the epoch, or an unreadable clock: claim the walk happened at
        // the end of time so every file looks racy and nothing is vouched for.
        .unwrap_or(i128::MAX)
}

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
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContextManifest {
    /// Canonical path of the entry file this walk started from.
    ///
    /// Every other field is relative to it: imports resolve against the entry's
    /// directory, so the same observations describe a different graph under a
    /// different anchor. The entry-chunk cache names files by entry *source*
    /// hash alone, deliberately, so two entries with identical bytes in
    /// different directories land on one cache file and each would otherwise
    /// find the other's manifest re-checking perfectly clean. See #5591.
    pub entry: PathBuf,
    /// Wall-clock nanoseconds when the walk that produced this manifest began.
    ///
    /// Stats alone cannot prove a file is unchanged if it was written at
    /// roughly the moment we looked at it: on a filesystem that stores mtime to
    /// the second, a write landing in the same tick as our read leaves the
    /// recorded identity byte-for-byte correct and already stale. Anchoring at
    /// walk *start* makes the rule conservative — anything whose mtime is not
    /// safely older than this could have changed under us.
    pub observed_at_ns: i128,
    pub files: Vec<ManifestFile>,
    pub unresolved: Vec<ManifestUnresolved>,
    pub unreadable: Vec<ManifestUnreadable>,
}

/// How far before [`ContextManifest::observed_at_ns`] a file's mtime must fall
/// before a stat proves anything.
///
/// Sized to the coarsest mtime granularity we can land on rather than to the
/// one we expect: FAT stores 2s, HFS+/ext3 and some NFS servers 1s, and
/// Windows' system clock ticks ~15.6ms. Being generous is close to free — it
/// only widens the window in which a manifest declines to vouch for a file
/// written moments ago, and declining costs one full walk that recomputes the
/// key anyway.
const RACY_MTIME_WINDOW_NS: i128 = 2_000_000_000;

/// Equality is over what the manifest *describes*, not when it was written.
///
/// `observed_at_ns` is metadata about the moment we looked, so deriving this
/// would make two manifests of a provably identical graph compare unequal for
/// having been walked microseconds apart — nondeterministic equality on a type
/// whose whole job is deciding whether something changed.
impl PartialEq for ContextManifest {
    fn eq(&self, other: &Self) -> bool {
        self.entry == other.entry
            && self.files == other.files
            && self.unresolved == other.unresolved
            && self.unreadable == other.unreadable
    }
}

impl Eq for ContextManifest {}

impl ContextManifest {
    /// An empty manifest anchored at `entry`, ready for a walk's observations.
    pub fn for_entry(entry: PathBuf) -> Self {
        Self {
            entry,
            observed_at_ns: now_ns(),
            files: Vec::new(),
            unresolved: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// Whether this manifest describes the graph reachable from `entry`, and
    /// that graph still looks exactly as it did when the manifest was written.
    ///
    /// Both halves are load-bearing. The observations alone prove only that
    /// some set of files is unchanged, not that it is *this* entry's set.
    ///
    /// Conservative in every direction: anything unreadable, ambiguous, or
    /// changed reports `false` and costs a walk.
    pub fn still_valid(&self, entry: &Path) -> bool {
        self.entry == entry
            && self
                .files
                .iter()
                .all(|file| file.still_valid(self.observed_at_ns))
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

    pub(crate) fn still_valid(&self, observed_at_ns: i128) -> bool {
        let Some((len, mtime_ns)) = module_source::stat_identity(&self.path) else {
            return false;
        };
        if len != self.len || mtime_ns != self.mtime_ns {
            return false;
        }
        // Matching stats are only evidence if they could not have been written
        // in the same unobservable tick as the walk. A file last modified at or
        // near the walk is racily clean: a later write inside that tick leaves
        // the identity we recorded intact, so believing it would serve a stale
        // chunk. Fall back to the full walk, which recomputes the key.
        if mtime_ns >= observed_at_ns.saturating_sub(RACY_MTIME_WINDOW_NS) {
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

    /// The entry every manifest in these tests is anchored at.
    ///
    /// They vary what the walk observed, not which entry it walked from, so one
    /// constant anchor keeps the anchor out of their way. What the anchor itself
    /// decides is pinned by `an_anchor_mismatch_invalidates` and, end to end,
    /// by `bytecode_cache_tests`.
    fn anchor() -> PathBuf {
        PathBuf::from("/harn/tests/entry.harn")
    }

    fn manifest_for(paths: &[PathBuf]) -> ContextManifest {
        ContextManifest {
            entry: anchor(),
            observed_at_ns: now_ns(),
            files: paths
                .iter()
                .map(|p| ManifestFile::observe(p).expect("observe"))
                .collect(),
            unresolved: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// Re-checks under the anchor the manifest was built at.
    fn revalidates(manifest: &ContextManifest) -> bool {
        manifest.still_valid(&anchor())
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

    /// Move `path`'s mtime `secs` into the past, clear of the racy window.
    ///
    /// A file a test just wrote is, correctly, too new to be vouched for. Tests
    /// about anything *other* than raciness have to age their fixtures first or
    /// they all collapse into the same assertion.
    fn backdate_mtime(path: &Path, secs: u64) {
        let current = std::fs::metadata(path).unwrap().modified().unwrap();
        set_mtime(path, current - std::time::Duration::from_secs(secs));
    }

    /// Comfortably outside [`RACY_MTIME_WINDOW_NS`].
    const SETTLED_SECS: u64 = 60;

    #[test]
    fn an_unchanged_graph_stays_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        // Aged past the racy window first: a file written this instant is
        // deliberately not vouched for, which is the test below.
        backdate_mtime(&dep, SETTLED_SECS);
        assert!(revalidates(&manifest_for(&[dep])));
    }

    #[test]
    fn a_file_written_in_the_same_tick_as_the_walk_is_not_vouched_for() {
        // The racily-clean rule. Nothing changes between the walk and the
        // re-check here, and the manifest still refuses -- because on a
        // filesystem with coarse mtime it *could* have changed without leaving
        // a trace, and the manifest cannot tell this host from that one.
        //
        // This is the half of the stat-only gap that shows up by accident:
        // codegen, formatters and agents all rewrite files far faster than a
        // 1s mtime tick. Declining costs one full walk; believing it costs a
        // silently stale chunk.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let manifest = manifest_for(&[dep.clone()]);
        assert_eq!(
            module_source::stat_identity(&dep),
            Some((manifest.files[0].len, manifest.files[0].mtime_ns)),
            "the stats must still match, or this proves nothing about raciness"
        );
        assert!(
            !revalidates(&manifest),
            "a file last written within the racy window must not be vouched for"
        );
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
            !revalidates(&manifest),
            "an edit of identical length must still invalidate the manifest"
        );
    }

    #[test]
    fn a_backdated_same_length_edit_is_the_residual_gap() {
        // What the racily-clean rule does NOT close, made executable so it is a
        // decision rather than a footnote: an edit that preserves length and
        // restores an mtime from well before the walk. The window rule cannot
        // catch this, because the file's timestamp is exactly the one a
        // genuinely untouched file would carry.
        //
        // The distinction from the test above is the whole point of the rule.
        // There, the collision was mechanical -- a write landing in the same
        // unobservable tick as the walk -- and that is now caught. Here it
        // takes deliberately restoring an old timestamp, which is not something
        // an editor, formatter or codegen step does by accident.
        //
        // This much is inherited, not introduced: `module_source`'s in-process
        // memo keys on the same `(len, mtime_ns)` identity and has the same
        // blind spot. If that identity is ever strengthened, this test should
        // fail and be deleted deliberately -- it exists so the trade-off cannot
        // move in either direction by accident.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 111 }\n");
        backdate_mtime(&dep, SETTLED_SECS);
        let original = std::fs::metadata(&dep).unwrap().modified().unwrap();
        let manifest = manifest_for(&[dep.clone()]);

        write(&dep, "pub fn v() -> int { return 222 }\n");
        set_mtime(&dep, original);
        assert_eq!(
            std::fs::metadata(&dep).unwrap().len(),
            33,
            "both versions must be the same byte length or this exercises \
             the length path instead of the mtime path"
        );

        assert!(
            revalidates(&manifest),
            "a stat-only manifest cannot notice an edit that preserves length \
             and restores a pre-walk mtime; if this now fails, the identity \
             was strengthened and this test should be removed on purpose"
        );
    }

    #[test]
    fn a_deleted_file_invalidates() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let manifest = manifest_for(&[dep.clone()]);
        std::fs::remove_file(&dep).unwrap();
        assert!(!revalidates(&manifest));
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
        backdate_mtime(&dep, SETTLED_SECS);
        let manifest = manifest_for(&[dep]);
        assert!(revalidates(&manifest));

        std::fs::create_dir(tmp.path().join("dep")).unwrap();
        assert!(
            !revalidates(&manifest),
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
            unresolved: vec![ManifestUnresolved {
                anchor: entry,
                import: "./late".to_string(),
            }],
            ..ContextManifest::for_entry(anchor())
        };
        assert!(revalidates(&manifest));

        write(
            &tmp.path().join("late.harn"),
            "pub fn l() -> int { return 1 }\n",
        );
        assert!(
            !revalidates(&manifest),
            "an import that now resolves must invalidate the manifest"
        );
    }

    #[test]
    fn an_anchor_mismatch_invalidates() {
        // Every observation can be immaculate and the manifest still describe
        // the wrong graph, because which files an import reaches depends on
        // where the walk started. Nothing else in the manifest can notice that:
        // the recorded paths are absolute and re-check clean from anywhere.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        backdate_mtime(&dep, SETTLED_SECS);
        let manifest = manifest_for(&[dep]);

        assert!(revalidates(&manifest), "unchanged under its own anchor");
        assert!(
            !manifest.still_valid(Path::new("/harn/tests/elsewhere/entry.harn")),
            "a manifest must not vouch for an entry it was not walked from"
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
