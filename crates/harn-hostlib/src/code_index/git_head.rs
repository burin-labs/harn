//! Best-effort `HEAD` SHA for a workspace root.
//!
//! Ordinary checkouts keep `HEAD` under `.git/`. Linked worktrees keep a
//! `.git` *file* with a `gitdir:` pointer, so reading `.git/HEAD` as a
//! nested path silently fails. Resolve the real git dir first, then read
//! `HEAD` from there, following `ref:` names into the worktree git dir or
//! the shared common dir.

use std::path::{Path, PathBuf};

/// Read the current `HEAD` SHA for `workspace_root`, if this is a git
/// checkout and the ref is resolvable as a loose file.
pub(super) fn read_git_head(workspace_root: &Path) -> Option<String> {
    let git_dir = git_dir(workspace_root)?;
    let txt = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let line = txt.trim().to_string();
    if let Some(ref_target) = line.strip_prefix("ref: ") {
        let in_git_dir = git_dir.join(ref_target);
        if let Ok(sha) = std::fs::read_to_string(&in_git_dir) {
            return Some(sha.trim().to_string());
        }
        if let Some(common) = read_commondir(&git_dir) {
            let in_common = common.join(ref_target);
            if let Ok(sha) = std::fs::read_to_string(&in_common) {
                return Some(sha.trim().to_string());
            }
        }
        return None;
    }
    Some(line)
}

fn git_dir(workspace_root: &Path) -> Option<PathBuf> {
    let git_path = workspace_root.join(".git");
    let meta = std::fs::symlink_metadata(&git_path).ok()?;
    if meta.is_dir() {
        return Some(git_path);
    }
    if meta.is_file() {
        return read_gitdir_file(&git_path);
    }
    None
}

fn read_gitdir_file(git_path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_path).ok()?;
    let raw = text.trim().strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_path.parent()?.join(candidate))
    }
}

fn read_commondir(git_dir: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_dir.join(candidate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn ordinary_checkout_reads_loose_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&root.join(".git/refs/heads/main"), "abc123def456\n");
        assert_eq!(read_git_head(root).as_deref(), Some("abc123def456"));
    }

    #[test]
    fn detached_head_returns_the_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join(".git/HEAD"), "deadbeefcafebabe\n");
        assert_eq!(read_git_head(root).as_deref(), Some("deadbeefcafebabe"));
    }

    #[test]
    fn linked_worktree_reads_head_through_gitdir_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let worktree = tmp.path().join("feature");
        let worktree_gitdir = main.join(".git/worktrees/feature");
        fs::create_dir_all(&worktree).unwrap();

        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&main.join(".git/refs/heads/main"), "aaaaaaaaaaaaaaaa\n");
        write(&main.join(".git/refs/heads/feature"), "bbbbbbbbbbbbbbbb\n");
        write(&worktree_gitdir.join("HEAD"), "ref: refs/heads/feature\n");
        write(&worktree_gitdir.join("commondir"), "../..\n");
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", worktree_gitdir.display()),
        );

        assert_eq!(
            read_git_head(&worktree).as_deref(),
            Some("bbbbbbbbbbbbbbbb")
        );
        assert_eq!(read_git_head(&main).as_deref(), Some("aaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn missing_git_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_git_head(tmp.path()), None);
    }
}
