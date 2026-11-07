//! Read-only Git worktree queries with explicit repository ownership.

use std::io::ErrorKind;
use std::path::Path;

/// Check whether a worktree-relative path is in Git's index.
///
/// A directory outside Git returns false. Unreadable repository metadata,
/// unavailable Git, and failed queries return errors rather than absence.
pub fn path_is_tracked(root: &Path, path: &Path) -> Result<bool, String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("Git tracked-path queries require a worktree-relative path".to_string());
    }
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("cannot resolve Git worktree {}: {error}", root.display()))?;
    if !may_be_git_worktree(&root) {
        return Ok(false);
    }
    let mut command = harn_vm::process_sandbox::session_std_command("git")
        .map_err(|error| format!("cannot prepare Git tracked-path query: {error}"))?;
    strip_ambient_git_env(&mut command);
    let output = command
        .args(["--literal-pathspecs", "-C"])
        .arg(&root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "ls-files",
            "--cached",
            "-z",
            "--",
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("cannot query Git's index: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot query Git's index in {} (exit {})",
            root.display(),
            output.status
        ));
    }
    Ok(!output.stdout.is_empty())
}

pub(crate) fn strip_ambient_git_env(command: &mut std::process::Command) {
    // Git's hook environment must not redirect an explicit worktree query.
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(&key);
        }
    }
}

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
