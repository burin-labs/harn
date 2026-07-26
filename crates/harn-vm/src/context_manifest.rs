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
//! Stats alone are not sufficient for a file written *while* the manifest was
//! being captured. Filesystems quantize mtime — two seconds on FAT, one second
//! on HFS+ and older NFS, a ~15.6ms clock tick on NTFS — so two writes inside
//! one tick record one mtime, and if the second preserves length the recorded
//! identity is byte-identical to the first. Programmatic agent edits land in
//! that window routinely. Each manifest therefore records when its capture
//! began, and an entry whose mtime is not a full granularity older than that
//! is "racily clean" in git's sense: judged by content instead of by stats
//! ([`crate::module_source::mtime_predates_capture`]). Re-checking a racy
//! entry also re-stamps the manifest, so the entry settles onto the stats-only
//! path on the next spawn rather than paying a read forever.
//!
//! The remaining gap is the one Cargo, Zig and Bazel accept: an edit that
//! preserves length *and* restores a settled mtime is not noticed, which takes
//! a deliberate timestamp forgery rather than an ordinary write. That gap is
//! pinned by a test rather than left to prose, so closing it — or widening it —
//! has to be a deliberate edit and cannot happen by accident.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::module_source::{self, ModuleSource};

/// One transitively reachable source file, plus the path that must stay absent
/// for the imports that reached it to keep resolving here.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestFile {
    /// Canonical path, matching the key the walk dedups on.
    pub path: PathBuf,
    pub len: u64,
    pub mtime_ns: i128,
    /// SHA-256 of the bytes that were folded into the context hash.
    ///
    /// Only read for an entry inside the racy window, where stats cannot
    /// decide. SHA-256 because the rest of the cache already identifies source
    /// by it — entry `source_hash`, module keys, artifact filenames — so this
    /// adds no second digest of the same bytes, and a warm process that has
    /// keyed the module artifact has already paid for it.
    pub content_hash: [u8; 32],
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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
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
    pub files: Vec<ManifestFile>,
    pub unresolved: Vec<ManifestUnresolved>,
    pub unreadable: Vec<ManifestUnreadable>,
    /// When this capture began, in [`module_source::stat_identity`]'s units and
    /// epoch. Every recorded stat was taken after this instant, which is what
    /// makes an older mtime provably un-reproducible by a later write.
    ///
    /// [`ContextManifest::begin`] is the only way to start one, so this is
    /// never absent. A manifest decoded from an artifact whose stamp is 0 —
    /// which no writer produces — classifies every entry as racy, costing
    /// reads rather than trusting stats that were never timestamped.
    pub captured_ns: i128,
}

/// What a re-check concluded about a manifest.
#[derive(Clone, Debug)]
pub enum ManifestCheck {
    /// The graph moved, or could not be proven unmoved. The caller must walk.
    Stale,
    /// Proven unchanged from stats alone.
    Valid,
    /// Proven unchanged, but at least one entry sat in the racy window and had
    /// to be read to decide. `refreshed` is the same manifest stamped with this
    /// re-check's capture time; persisting it settles those entries onto the
    /// stats-only path, the way git rewrites a racily-clean index entry rather
    /// than re-reading it on every command.
    ValidAfterRecheck { refreshed: ContextManifest },
}

impl ContextManifest {
    /// Begin a capture anchored at `entry`, stamping the time *before*
    /// anything is observed.
    ///
    /// The order matters: a stat taken before the stamp could miss a write that
    /// landed between the two and still be called settled.
    pub fn begin(entry: PathBuf) -> Self {
        Self {
            entry,
            captured_ns: module_source::now_ns(),
            files: Vec::new(),
            unresolved: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// Whether this manifest describes the graph reachable from `entry`, and
    /// that graph still looks exactly as it did when the manifest was written.
    ///
    /// Conservative in every direction: anything unreadable, ambiguous, or
    /// changed reports `false` and costs a walk.
    pub fn still_valid(&self, entry: &Path) -> bool {
        !matches!(self.check(entry), ManifestCheck::Stale)
    }

    /// As [`Self::still_valid`], but also reports whether the answer needed a
    /// content read, so a caller holding the artifact can re-stamp it.
    pub fn check(&self, entry: &Path) -> ManifestCheck {
        // The anchor first: it is a comparison, where everything below is at
        // least a stat. The observations prove only that some set of files is
        // unchanged, never that it is *this* entry's set (#5591).
        if self.entry != entry {
            return ManifestCheck::Stale;
        }
        // Sampled before the stats below, so the refreshed stamp is as
        // trustworthy as one a fresh walk would produce.
        let captured_ns = module_source::now_ns();
        let mut rechecked = false;
        for file in &self.files {
            match file.check(self.captured_ns) {
                FileCheck::Stale => return ManifestCheck::Stale,
                FileCheck::Settled => {}
                FileCheck::Rechecked => rechecked = true,
            }
        }
        if !self
            .unresolved
            .iter()
            .all(ManifestUnresolved::still_unresolved)
            || !self
                .unreadable
                .iter()
                .all(ManifestUnreadable::still_unreadable)
        {
            return ManifestCheck::Stale;
        }
        if rechecked {
            ManifestCheck::ValidAfterRecheck {
                refreshed: Self {
                    captured_ns,
                    ..self.clone()
                },
            }
        } else {
            ManifestCheck::Valid
        }
    }
}

/// What a re-check concluded about one recorded file.
enum FileCheck {
    /// Stats matched and the entry was old enough for stats to be proof.
    Settled,
    /// Stats matched but the entry was racily clean, and its content confirmed
    /// it.
    Rechecked,
    Stale,
}

impl ManifestFile {
    /// Record `path` as observed on disk now, carrying the digest of the
    /// `source` the walk folded into the context hash. `None` if the file
    /// cannot be stat'ed — a file we cannot describe is one we must not claim
    /// is unchanged.
    ///
    /// The digest comes from the caller's already-read bytes rather than from a
    /// re-read here: it has to describe the version that went into the hash,
    /// not whatever a second read would find.
    pub fn observe(path: &Path, source: &ModuleSource) -> Option<Self> {
        let (len, mtime_ns) = module_source::stat_identity(path)?;
        Some(Self {
            path: path.to_path_buf(),
            len,
            mtime_ns,
            content_hash: source.sha256(),
            shadow: shadow_path(path),
        })
    }

    fn check(&self, captured_ns: i128) -> FileCheck {
        let Some((len, mtime_ns)) = module_source::stat_identity(&self.path) else {
            return FileCheck::Stale;
        };
        if len != self.len || mtime_ns != self.mtime_ns {
            return FileCheck::Stale;
        }
        if self.shadow.as_ref().is_some_and(|shadow| shadow.exists()) {
            return FileCheck::Stale;
        }
        if module_source::mtime_predates_capture(mtime_ns, captured_ns) {
            return FileCheck::Settled;
        }
        if self.content_matches() {
            FileCheck::Rechecked
        } else {
            FileCheck::Stale
        }
    }

    /// Whether the file still holds the bytes whose digest was recorded.
    ///
    /// Deliberately not [`module_source::read`]: that memo is keyed on the very
    /// `(path, len, mtime_ns)` triple this entry just failed to trust, so a hit
    /// would answer with the same bytes the racy window let through. Reading as
    /// a string mirrors how the recorded digest was produced, so a file that
    /// stopped being UTF-8 reads as changed.
    fn content_matches(&self) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return false;
        };
        // A direct read, deliberately not `module_source::read`: that memo is
        // keyed on the very `(len, mtime_ns)` identity this entry has already
        // been shown to reproduce, so it would hand back the stale bytes and
        // agree with itself.
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        digest == self.content_hash
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
            files: paths
                .iter()
                .map(|p| {
                    let source = ModuleSource::from_text(std::fs::read_to_string(p).unwrap());
                    ManifestFile::observe(p, &source).expect("observe")
                })
                .collect(),
            ..ContextManifest::begin(anchor())
        }
    }

    /// Re-checks under the anchor the manifest was built at.
    fn revalidates(manifest: &ContextManifest) -> bool {
        manifest.still_valid(&anchor())
    }

    /// Stamp `path`'s mtime. Every timestamp this module reasons about is
    /// controlled outright rather than waited for, so the tests neither sleep
    /// nor depend on how finely the host filesystem happens to keep time.
    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    fn mtime_of(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    /// A manifest over files old enough that stats alone are proof — the
    /// ordinary case, and the one the fast path exists for.
    fn settled_manifest_for(paths: &[PathBuf]) -> ContextManifest {
        for path in paths {
            // Relative to the file's own mtime, so the ageing is expressed in
            // the filesystem's clock rather than a second reading of the host's.
            set_mtime(path, mtime_of(path) - std::time::Duration::from_hours(1));
        }
        let manifest = manifest_for(paths);
        for file in &manifest.files {
            assert!(
                module_source::mtime_predates_capture(file.mtime_ns, manifest.captured_ns),
                "{} must be outside the racy window for this helper to mean anything",
                file.path.display()
            );
        }
        manifest
    }

    /// Put `manifest`'s capture in the same timestamp tick as its first entry's
    /// mtime — what a coarse filesystem does on its own (NTFS quantizes to a
    /// ~15.6ms clock tick, FAT to two seconds) and what a nanosecond-resolution
    /// APFS or ext4 would essentially never produce, which is why the tests
    /// state it rather than hoping for it.
    fn place_inside_racy_window(manifest: &mut ContextManifest) {
        manifest.captured_ns = manifest.files[0].mtime_ns;
        assert!(
            !module_source::mtime_predates_capture(
                manifest.files[0].mtime_ns,
                manifest.captured_ns
            ),
            "the entry must be racily clean for the guard to be under test"
        );
    }

    #[test]
    fn an_unchanged_graph_stays_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        assert!(revalidates(&manifest_for(&[dep])));
    }

    #[test]
    fn a_same_length_edit_invalidates() {
        // Two writes inside one filesystem timestamp tick record one mtime, so
        // when the second preserves byte length the recorded `(len, mtime_ns)`
        // is identical and stats have nothing left to notice. Windows hits this
        // routinely; the agent edits Harn exists to orchestrate are exactly the
        // fast programmatic writes that land inside a tick. The capture time is
        // what makes it decidable: an entry that was not already settled when
        // the manifest was taken is judged by content.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 111 }\n");
        let mut manifest = manifest_for(&[dep.clone()]);
        place_inside_racy_window(&mut manifest);

        let recorded = mtime_of(&dep);
        write(&dep, "pub fn v() -> int { return 222 }\n");
        set_mtime(&dep, recorded);
        assert_eq!(
            module_source::stat_identity(&dep).unwrap(),
            (manifest.files[0].len, manifest.files[0].mtime_ns),
            "the edit must leave a byte-identical stat identity, or this test \
             would pass on the stats path and prove nothing"
        );

        assert!(
            !revalidates(&manifest),
            "an edit of identical length must still invalidate the manifest"
        );
    }

    #[test]
    fn a_settled_entry_is_proven_by_stats_without_reading_content() {
        // The counterweight to the racy-window check: the overwhelming majority
        // of entries are old enough that stats decide, and they must not start
        // paying a read. Proven by recording a digest that cannot match — a
        // check that consulted content would reject this manifest, and one that
        // needed a re-stamp would report `ValidAfterRecheck`.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let mut manifest = settled_manifest_for(&[dep]);
        manifest.files[0].content_hash = [0xAB; 32];

        assert!(
            matches!(manifest.check(&anchor()), ManifestCheck::Valid),
            "a settled entry must be decided by stats alone"
        );
    }

    #[test]
    fn an_edit_that_restores_a_settled_mtime_is_the_documented_gap() {
        // The stated limit of the whole scheme, made executable so it stays a
        // decision rather than a footnote. Once an entry is settled, stats are
        // treated as proof and content is never consulted — so an edit that
        // preserves length and puts the old, already-settled mtime back is not
        // noticed. Unlike the racy window this replaces, that takes deliberate
        // timestamp forgery rather than an ordinary fast write, which is the
        // same line Cargo, Zig and Bazel draw.
        //
        // If this ever fails, the identity was strengthened and this test
        // should be deleted on purpose, not repaired.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 111 }\n");
        let manifest = settled_manifest_for(&[dep.clone()]);

        let settled = mtime_of(&dep);
        write(&dep, "pub fn v() -> int { return 222 }\n");
        set_mtime(&dep, settled);
        assert_eq!(
            module_source::stat_identity(&dep).unwrap(),
            (manifest.files[0].len, manifest.files[0].mtime_ns),
            "the forgery must leave a byte-identical stat identity, or this \
             test proves nothing"
        );

        assert!(
            revalidates(&manifest),
            "a settled entry is decided by stats, so a forged timestamp is not \
             noticed; if this now fails the trade-off changed and the test \
             should be removed deliberately"
        );
    }

    #[test]
    fn a_racy_entry_that_still_matches_settles_instead_of_re_reading_forever() {
        // A racily clean entry that checks out is not a miss: the graph is
        // unchanged, and re-stamping the manifest with this check's capture
        // time moves the entry onto the stats path for later spawns. Without
        // that, every future spawn would re-read the same file.
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("dep.harn");
        write(&dep, "pub fn v() -> int { return 1 }\n");
        let mut manifest = manifest_for(&[dep.clone()]);
        place_inside_racy_window(&mut manifest);

        // Rewritten with the same bytes and stamped back, so only content can
        // tell this apart from the edit above.
        let recorded = mtime_of(&dep);
        write(&dep, "pub fn v() -> int { return 1 }\n");
        set_mtime(&dep, recorded);

        let ManifestCheck::ValidAfterRecheck { refreshed } = manifest.check(&anchor()) else {
            panic!("a racy entry whose content matches must validate, and report the re-check");
        };
        assert!(
            refreshed.captured_ns > manifest.captured_ns,
            "the re-stamped manifest must carry the newer capture"
        );
        assert_eq!(
            refreshed.files, manifest.files,
            "re-stamping must not disturb the observations themselves"
        );

        // A later spawn re-checks the re-stamped manifest, and once its capture
        // is a full granularity past the write the entry is decided by stats.
        // Advancing the recorded capture stands in for that elapsed time rather
        // than sleeping through it.
        let later = ContextManifest {
            captured_ns: refreshed.captured_ns + module_source::TIMESTAMP_GRANULARITY_NS,
            ..refreshed
        };
        assert!(
            matches!(later.check(&anchor()), ManifestCheck::Valid),
            "an entry the racy window has moved past must return to the stats path"
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
            ..ContextManifest::begin(anchor())
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
        let body = "pub fn v() -> int { return 1 }\n";
        write(&odd, body);
        let source = ModuleSource::from_text(body);
        assert_eq!(ManifestFile::observe(&odd, &source).unwrap().shadow, None);
    }
}
