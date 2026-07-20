//! The single upward "where does my project start" walk for every Harn
//! frontend.
//!
//! Locating the nearest `harn.toml` — the question "which project governs this
//! file?" — was answered by nine hand-rolled loops (in the CLI, the LSP,
//! `harn doctor`, the MCP command, the VM runtime, …) that disagreed on the
//! stop conditions. Some stopped at a `.git` boundary; others walked to the
//! filesystem root and would silently adopt a stray `harn.toml` in `$HOME`.
//! Some matched a *directory* named `harn.toml`; some parsed relative start
//! paths against the wrong base. This module is the one walk they now all
//! share, so every frontend answers the question identically.
//!
//! The walk, from `start` toward the filesystem root:
//! - normalizes `start` to an absolute path against the working directory, so
//!   a relative or not-yet-existing start still resolves against real
//!   ancestors;
//! - begins at `start` itself when it is a directory, otherwise at its parent;
//! - matches only a *regular file* of the target name, never a directory;
//! - stops at the first `.git` boundary, so a manifest in an enclosing project
//!   or in `$HOME` is never adopted across a repository boundary;
//! - inspects at most [`MAX_PARENT_DIRS`] directories, bounding pathological
//!   paths (the parent chain strictly shortens, so a symlink cannot make it
//!   loop).
//!
//! The traversal is **lexical**: it ascends the parent chain of the path as
//! written and returns paths in that same identity. It deliberately does *not*
//! canonicalize, because the returned project root feeds lexical path-prefix
//! guards elsewhere (workspace-root write guards, sandbox roots); flipping the
//! returned identity (e.g. macOS `/var` → `/private/var`) would desync those
//! prefix checks. Symlinks are still resolved *where it matters*: the `.git`
//! and target-file probes stat through symlinks at the OS layer, so the stop
//! and match decisions are physically correct even though the returned path is
//! lexical. A wholesale canonical cutover — with every prefix-guard site
//! audited — is a deliberate separate change, not a side effect of this
//! consolidation.

use std::path::{Path, PathBuf};

/// Filename of the Harn project manifest.
pub const MANIFEST_FILENAME: &str = "harn.toml";

/// Hard cap on how many ancestor directories the walk inspects.
///
/// The walk normally stops earlier, at the first `.git` boundary; this cap
/// bounds the ascent in a very deep tree that has no `.git` at all. (The walk
/// cannot loop: the lexical parent chain strictly shortens each step.)
pub(crate) const MAX_PARENT_DIRS: usize = 16;

/// A file located by [`find_nearest_ancestor`].
///
/// Named fields rather than a tuple: the walk produces both halves together,
/// and one of the hand-rolled copies this replaces returned a bare
/// `(PathBuf, PathBuf)` whose two positions are impossible to tell apart at a
/// call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFile {
    /// Absolute path to the located file itself.
    pub path: PathBuf,
    /// Directory containing the file — the project root.
    pub dir: PathBuf,
}

/// Walk up from `start` looking for the nearest ancestor directory that
/// directly contains a regular file named `filename`.
///
/// `filename` may be a multi-component relative path (e.g.
/// `.harn/package-current.toml`); it is joined onto each candidate directory.
///
/// See the [module docs](self) for the exact normalization and stop
/// conditions. Returns `None` when no match is found before a stop condition.
pub fn find_nearest_ancestor(start: &Path, filename: impl AsRef<Path>) -> Option<FoundFile> {
    let filename = filename.as_ref();
    // Normalize to an absolute path so the walk works when `start` is a
    // relative or not-yet-existing path. Kept lexical on purpose — see the
    // module docs on why the returned identity must not be canonicalized.
    let base = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(start)
    };

    let mut cursor: Option<PathBuf> = if base.is_dir() {
        Some(base)
    } else {
        base.parent().map(Path::to_path_buf)
    };

    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let candidate = dir.join(filename);
        if candidate.is_file() {
            return Some(FoundFile {
                path: candidate,
                dir,
            });
        }
        // Stop at a `.git` boundary so a stray manifest in a parent project or
        // in `$HOME` is never silently picked up.
        if dir.join(".git").exists() {
            break;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }

    None
}

/// Walk up from `start` to the nearest `harn.toml`, returning the manifest
/// path and the directory that holds it.
pub fn find_nearest_manifest(start: &Path) -> Option<FoundFile> {
    find_nearest_ancestor(start, MANIFEST_FILENAME)
}

/// The project root governing `start`: the directory of the nearest
/// `harn.toml`, or `None` when there is none before a stop condition.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    find_nearest_manifest(start).map(|found| found.dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn reports_both_the_manifest_and_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        touch(&root.join("harn.toml"));
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_nearest_manifest(&nested).expect("manifest found from a nested dir");
        assert_eq!(found.path, root.join("harn.toml"));
        assert_eq!(found.dir, root);
    }

    #[test]
    fn a_file_start_begins_at_its_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        touch(&root.join("harn.toml"));
        let file = root.join("main.harn");
        touch(&file);

        let found = find_nearest_manifest(&file).expect("manifest found from a file path");
        assert_eq!(found.dir, root);
    }

    #[test]
    fn stops_at_the_git_boundary() {
        // The manifest above `.git` belongs to a different project (or to
        // `$HOME`). The walk must refuse it — this is the divergence that made
        // the LSP and `harn doctor` disagree with the CLI.
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().to_path_buf();
        touch(&outer.join("harn.toml"));
        let project = outer.join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let src = project.join("src");
        std::fs::create_dir_all(&src).unwrap();

        assert_eq!(
            find_nearest_manifest(&src),
            None,
            "must not reach across a .git boundary"
        );
    }

    #[test]
    fn ignores_a_directory_named_like_the_manifest() {
        // A *directory* named `harn.toml` must not be mistaken for a manifest.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("harn.toml")).unwrap();

        assert_eq!(find_nearest_manifest(&root), None);
    }

    #[test]
    fn yields_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        // Anchor the root with a `.git` so the result cannot depend on whatever
        // sits above $TMPDIR on the host.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("a");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_nearest_manifest(&nested), None);
    }

    #[test]
    fn arbitrary_sentinel_filename_parameterizes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        touch(&root.join(".harn").join("package-current.toml"));
        let nested = root.join("pkg").join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_nearest_ancestor(&nested, ".harn/package-current.toml")
            .expect("multi-component sentinel resolves");
        assert_eq!(found.dir, root);
    }
}
