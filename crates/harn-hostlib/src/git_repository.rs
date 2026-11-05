use std::io::ErrorKind;
use std::path::Path;

/// Only a fully observed absence rules out Git; unreadable metadata stays unknown.
pub(crate) fn may_be_git_worktree(root: &Path) -> bool {
    if !root.is_dir() {
        return true;
    }
    for directory in root.ancestors() {
        match std::fs::symlink_metadata(directory.join(".git")) {
            Ok(_) => return true,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_markers_are_absent_but_git_files_and_parent_markers_are_candidates() {
        let root = tempfile::tempdir().unwrap();
        assert!(!may_be_git_worktree(root.path()));
        std::fs::write(root.path().join(".git"), "gitdir: /missing/worktree\n").unwrap();
        assert!(may_be_git_worktree(root.path()));
        assert!(may_be_git_worktree(&root.path().join("nested")));
    }

    #[test]
    fn a_metadata_read_error_does_not_claim_repository_absence() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, "not a directory").unwrap();
        assert!(may_be_git_worktree(&file));
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_git_pointer_remains_a_candidate_for_git_to_diagnose() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(root.path().join("missing"), root.path().join(".git")).unwrap();
        assert!(may_be_git_worktree(root.path()));
    }
}
