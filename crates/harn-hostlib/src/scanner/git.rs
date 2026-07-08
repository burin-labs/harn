//! Git-backed scanner inputs.
//!
//! The scanner core consumes Git through this capability boundary so tests
//! can exercise tracked-file and churn behavior without depending on the
//! ambient checkout, Git hooks, fsmonitor, or hook-time environment state.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Git data needed by the scanner.
pub trait GitCapabilities {
    /// Return tracked and untracked file paths relative to `root`.
    fn list_files(&self, root: &Path) -> Option<Vec<String>>;

    /// Return normalized 0..1 churn scores keyed by paths relative to `root`.
    fn churn_scores(&self, root: &Path) -> BTreeMap<String, f64>;
}

/// Production [`GitCapabilities`] implementation backed by the `git` CLI.
#[derive(Debug, Default)]
pub struct CliGitCapabilities;

impl GitCapabilities for CliGitCapabilities {
    fn list_files(&self, root: &Path) -> Option<Vec<String>> {
        if !has_git_repository_marker(root) {
            return None;
        }

        let mut cmd = Command::new("git");
        super::strip_ambient_git_env(&mut cmd);
        let output = cmd
            .args([
                "-C",
                root.to_str()?,
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8(output.stdout).ok()?;
        let entries: Vec<String> = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(entries)
        }
    }

    fn churn_scores(&self, root: &Path) -> BTreeMap<String, f64> {
        if !has_git_repository_marker(root) {
            return BTreeMap::new();
        }

        let mut cmd = Command::new("git");
        super::strip_ambient_git_env(&mut cmd);
        let output = cmd
            .args([
                "-C",
                match root.to_str() {
                    Some(s) => s,
                    None => return BTreeMap::new(),
                },
                "log",
                "--since=90.days",
                "--name-only",
                "--pretty=format:",
            ])
            .output();
        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return BTreeMap::new(),
        };
        let stdout = match String::from_utf8(output.stdout) {
            Ok(s) => s,
            Err(_) => return BTreeMap::new(),
        };

        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            *counts.entry(trimmed.to_string()).or_insert(0) += 1;
        }

        let max = counts.values().copied().max().unwrap_or(1).max(1) as f64;
        counts
            .into_iter()
            .map(|(file, count)| (file, count as f64 / max))
            .collect()
    }
}

/// Returns true when `root` is inside a Git worktree based on local marker files.
///
/// This deliberately avoids shelling out to `git rev-parse`: the default
/// capability uses this predicate to decide whether spawning Git is appropriate.
fn has_git_repository_marker(root: &Path) -> bool {
    root.ancestors().any(|dir| dir.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn tempdir_outside_ambient_repo() -> tempfile::TempDir {
        let ambient = std::env::current_dir().expect("cwd");
        let temp_root = std::env::temp_dir();
        let ambient_repo = ambient
            .ancestors()
            .find(|path| path.join(".git").exists())
            .unwrap_or(ambient.as_path());
        if !temp_root.starts_with(ambient_repo) {
            return tempdir().unwrap();
        }
        let parent = ambient_repo
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(".harn-hostlib-test-tmp");
        fs::create_dir_all(&parent).unwrap();
        tempfile::Builder::new()
            .prefix("harn-hostlib-git-")
            .tempdir_in(parent)
            .unwrap()
    }

    #[test]
    fn marker_detection_handles_plain_and_worktree_git_markers() {
        let tmp = tempdir_outside_ambient_repo();
        let root = tmp.path();

        assert!(!has_git_repository_marker(root));

        fs::write(root.join(".git"), "gitdir: /tmp/example\n").unwrap();
        assert!(has_git_repository_marker(root));
        assert!(has_git_repository_marker(&root.join("nested")));
    }
}
