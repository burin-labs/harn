//! Git repository topology resolution shared by git-remote detection and
//! sandbox scope assembly.
//!
//! `harn run` confines subprocesses to the workspace roots. Git, however, keeps
//! parts of a repository *outside* the working tree in two common, first-class
//! setups:
//!
//! * a **linked worktree** (`git worktree add`) whose `.git` is a file pointing
//!   at `.../.git/worktrees/<name>` in the main repository, with a `commondir`
//!   file linking back to the shared `.git`;
//! * a repository whose `.git/objects/info/alternates` borrows objects from
//!   another repo's object store (`git clone --shared`, dissociated grafts).
//!
//! When those external directories fall outside the sandboxed roots, every git
//! subprocess fails (`fatal: not a git repository`, `error: unable to open
//! object pack directory: ... Operation not permitted`) and Harn's `std/git`
//! builtins return empty output inside a perfectly ordinary checkout. Git
//! worktrees are a first-class Harn/Burin workflow (Burin's own agents run in
//! worktrees), so [`git_scope_extension`] resolves the extra directories git
//! legitimately needs and the sandbox layer extends its filesystem scope with
//! them (see `stdlib::sandbox`).
//!
//! Every resolver here is defensive: a missing or malformed file yields "no
//! extension", never an error, so a non-git directory (or a partially written
//! one) simply contributes nothing.

use std::path::{Path, PathBuf};

/// Maximum depth of `objects/info/alternates` chains to follow. A borrowed
/// object store can itself borrow, but git bounds alternate nesting; this cap
/// keeps a malformed or cyclic chain from looping.
const MAX_ALTERNATES_DEPTH: usize = 5;

/// The external directories a git repository rooted at a workspace root needs
/// the sandbox to expose, split by the access git actually requires.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GitScopeExtension {
    /// The real git directory and shared common dir of a linked worktree: git
    /// writes locks, the index, refs, and reflogs there, so these need
    /// read-write scope. Empty for an ordinary in-tree `.git` directory, which
    /// the workspace root already covers.
    pub(crate) read_write: Vec<PathBuf>,
    /// Object directories borrowed through `objects/info/alternates`: git only
    /// ever reads borrowed objects, so read-only scope suffices.
    pub(crate) read_only: Vec<PathBuf>,
}

/// Resolve the external git directories the sandbox must expose for a workspace
/// `root`. Returns an empty extension for a non-git directory, an ordinary
/// in-tree checkout with no alternates, or any malformed git metadata.
pub(crate) fn git_scope_extension(root: &Path) -> GitScopeExtension {
    let mut ext = GitScopeExtension::default();
    let git_path = root.join(".git");
    let Some(common_dir) = resolve_common_dir(&git_path, &mut ext.read_write) else {
        return ext;
    };
    collect_alternates(&common_dir, &mut ext.read_only, 0);
    ext
}

/// Resolve the git *common* directory for `<root>/.git`, pushing any external
/// directories that need read-write scope (the worktree gitdir and the shared
/// common dir) onto `read_write`. Returns the common dir whose `objects` store
/// alternates are read from, or `None` when there is no usable git metadata.
fn resolve_common_dir(git_path: &Path, read_write: &mut Vec<PathBuf>) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(git_path).ok()?;
    if meta.is_dir() {
        // Ordinary repository: `.git` lives inside the workspace root and is
        // already in scope. Only its alternates (if any) can point outside.
        return Some(git_path.to_path_buf());
    }
    // Linked worktree: `.git` is a file pointing at the real git dir, which
    // lives outside the working tree.
    let git_dir = read_gitdir_file(git_path)?;
    push_existing_dir(read_write, &git_dir);
    let common_dir = read_commondir(&git_dir).unwrap_or_else(|| git_dir.clone());
    push_existing_dir(read_write, &common_dir);
    Some(common_dir)
}

/// Follow `<common_dir>/objects/info/alternates`, pushing each borrowed object
/// directory onto `read_only` and recursing into its own alternates up to
/// [`MAX_ALTERNATES_DEPTH`].
fn collect_alternates(common_dir: &Path, read_only: &mut Vec<PathBuf>, depth: usize) {
    if depth >= MAX_ALTERNATES_DEPTH {
        return;
    }
    let objects_dir = common_dir.join("objects");
    let Some(text) = read_text_if_exists(&objects_dir.join("info").join("alternates")) else {
        return;
    };
    for line in text.lines() {
        let entry = line.trim();
        // Git treats blank lines as ignorable and lines beginning with '#' as
        // comments in the alternates file.
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let candidate = PathBuf::from(entry);
        // Relative alternates are resolved against the borrowing repo's own
        // `objects` directory, matching git's own interpretation.
        let alt_objects = if candidate.is_absolute() {
            candidate
        } else {
            objects_dir.join(candidate)
        };
        if !push_existing_dir(read_only, &alt_objects) {
            continue;
        }
        // The alternate's own alternates live at `<alt_objects>/info/alternates`.
        // Recurse using the git dir that owns that objects store so the join in
        // the recursive call reconstructs the same objects path.
        if let Some(alt_git_dir) = alt_objects.parent() {
            collect_alternates(alt_git_dir, read_only, depth + 1);
        }
    }
}

/// Parse the `gitdir: <path>` pointer of a linked worktree's `.git` file.
/// Relative targets resolve against the `.git` file's directory. Returns `None`
/// for an unreadable file or one that does not begin with `gitdir:`.
pub(crate) fn read_gitdir_file(git_path: &Path) -> Option<PathBuf> {
    let text = read_text_if_exists(git_path)?;
    let raw = text.trim().strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_path.parent()?.join(candidate))
    }
}

/// Resolve the shared common git dir from a worktree git dir's `commondir`
/// file. Relative targets resolve against the git dir. Returns `None` when the
/// file is absent (the git dir is itself the common dir).
pub(crate) fn read_commondir(git_dir: &Path) -> Option<PathBuf> {
    let raw = read_text_if_exists(&git_dir.join("commondir"))?;
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_dir.join(candidate))
    }
}

/// Push `candidate` onto `dirs` when it resolves to an existing directory and
/// is not already present, returning whether it was accepted. Following the
/// pointer to an existing directory keeps a dangling or malformed reference
/// from widening the sandbox to a path git could not use anyway.
fn push_existing_dir(dirs: &mut Vec<PathBuf>, candidate: &Path) -> bool {
    if !candidate.is_dir() {
        return false;
    }
    if !dirs.iter().any(|existing| existing == candidate) {
        dirs.push(candidate.to_path_buf());
    }
    true
}

fn read_text_if_exists(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn ordinary_repo_yields_no_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        let ext = git_scope_extension(root);
        assert!(ext.read_write.is_empty());
        assert!(ext.read_only.is_empty());
    }

    #[test]
    fn non_git_directory_yields_no_extension() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            git_scope_extension(tmp.path()),
            GitScopeExtension::default()
        );
    }

    #[test]
    fn linked_worktree_resolves_gitdir_and_commondir() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let git_common = main.join(".git");
        let worktree_gitdir = git_common.join("worktrees/feature");
        std::fs::create_dir_all(git_common.join("objects")).unwrap();
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        // commondir is relative to the worktree git dir.
        write(&worktree_gitdir.join("commondir"), "../..\n");

        let worktree = tmp.path().join("feature");
        std::fs::create_dir_all(&worktree).unwrap();
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", worktree_gitdir.display()),
        );

        let ext = git_scope_extension(&worktree);
        assert!(
            ext.read_write.iter().any(|p| p == &worktree_gitdir),
            "expected worktree gitdir in {:?}",
            ext.read_write
        );
        // `<worktree_gitdir>/../..` normalizes to `<main>/.git`.
        let resolved_common = worktree_gitdir.join("../..");
        assert!(
            ext.read_write.iter().any(|p| p == &resolved_common),
            "expected common dir in {:?}",
            ext.read_write
        );
    }

    #[test]
    fn worktree_with_relative_gitdir_resolves_against_dot_git_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git_common = root.join(".git");
        let worktree_gitdir = git_common.join("worktrees/feature");
        std::fs::create_dir_all(git_common.join("objects")).unwrap();
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        write(&worktree_gitdir.join("commondir"), "../..\n");

        let worktree = root.join("feature");
        std::fs::create_dir_all(&worktree).unwrap();
        // Relative gitdir pointer, resolved against the `.git` file's directory.
        write(
            &worktree.join(".git"),
            "gitdir: ../.git/worktrees/feature\n",
        );

        let ext = git_scope_extension(&worktree);
        assert!(
            ext.read_write
                .iter()
                .any(|p| p == &worktree.join("../.git/worktrees/feature")),
            "expected relative gitdir resolved in {:?}",
            ext.read_write
        );
    }

    #[test]
    fn absolute_alternates_are_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git/objects/info")).unwrap();
        let shared_objects = tmp.path().join("source/.git/objects");
        std::fs::create_dir_all(&shared_objects).unwrap();
        write(
            &root.join(".git/objects/info/alternates"),
            &format!("{}\n", shared_objects.display()),
        );

        let ext = git_scope_extension(&root);
        assert!(ext.read_write.is_empty());
        assert_eq!(ext.read_only, vec![shared_objects]);
    }

    #[test]
    fn relative_alternates_resolve_against_objects_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git/objects/info")).unwrap();
        let shared_objects = tmp.path().join("source/.git/objects");
        std::fs::create_dir_all(&shared_objects).unwrap();
        // Relative to `<repo>/.git/objects`.
        write(
            &root.join(".git/objects/info/alternates"),
            "../../../source/.git/objects\n",
        );

        let ext = git_scope_extension(&root);
        let expected = root
            .join(".git/objects")
            .join("../../../source/.git/objects");
        assert_eq!(ext.read_only, vec![expected]);
    }

    #[test]
    fn alternates_blank_and_comment_lines_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git/objects/info")).unwrap();
        let shared_objects = tmp.path().join("source/.git/objects");
        std::fs::create_dir_all(&shared_objects).unwrap();
        write(
            &root.join(".git/objects/info/alternates"),
            &format!("# a comment\n\n{}\n", shared_objects.display()),
        );

        let ext = git_scope_extension(&root);
        assert_eq!(ext.read_only, vec![shared_objects]);
    }

    #[test]
    fn nonexistent_alternate_target_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git/objects/info")).unwrap();
        write(
            &root.join(".git/objects/info/alternates"),
            "/does/not/exist/objects\n",
        );

        let ext = git_scope_extension(&root);
        assert!(ext.read_only.is_empty());
    }

    #[test]
    fn chained_alternates_are_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git/objects/info")).unwrap();

        let mid_objects = tmp.path().join("mid/.git/objects");
        std::fs::create_dir_all(mid_objects.join("info")).unwrap();
        let base_objects = tmp.path().join("base/.git/objects");
        std::fs::create_dir_all(&base_objects).unwrap();

        write(
            &root.join(".git/objects/info/alternates"),
            &format!("{}\n", mid_objects.display()),
        );
        write(
            &mid_objects.join("info/alternates"),
            &format!("{}\n", base_objects.display()),
        );

        let ext = git_scope_extension(&root);
        assert!(ext.read_only.contains(&mid_objects));
        assert!(ext.read_only.contains(&base_objects));
    }

    #[test]
    fn malformed_gitdir_file_yields_no_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("feature");
        std::fs::create_dir_all(&worktree).unwrap();
        write(&worktree.join(".git"), "this is not a gitdir pointer\n");
        assert_eq!(git_scope_extension(&worktree), GitScopeExtension::default());
    }

    #[test]
    fn dangling_gitdir_pointer_is_not_added() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("feature");
        std::fs::create_dir_all(&worktree).unwrap();
        write(&worktree.join(".git"), "gitdir: /nonexistent/git/dir\n");
        let ext = git_scope_extension(&worktree);
        assert!(ext.read_write.is_empty());
        assert!(ext.read_only.is_empty());
    }
}
