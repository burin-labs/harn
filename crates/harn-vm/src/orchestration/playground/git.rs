//! Thin wrappers around the host `git` binary used by the Merge Captain
//! mock-repos playground (#1020).
//!
//! The playground exercises *real* git: bare remotes plus working clones,
//! feature branches with overlay commits, force-with-lease pushes,
//! and a real merge commit when a PR is "merged". Keeping the wrappers
//! tiny and explicit makes the surface easy to audit and to swap to
//! `gix` later if we ever drop the binary dependency.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::value::VmError;

pub struct GitOps {
    pub author_name: String,
    pub author_email: String,
    /// Pinned timestamp for `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE` so
    /// commit SHAs are reproducible across runs from the same manifest.
    /// `2026-01-01T00:00:00 +0000` aligns with `PlaygroundState::now_ms`.
    pub committer_date: String,
}

impl Default for GitOps {
    fn default() -> Self {
        GitOps {
            author_name: "Playground Bot".to_string(),
            author_email: "bot@playground.invalid".to_string(),
            committer_date: "2026-01-01T00:00:00 +0000".to_string(),
        }
    }
}

impl GitOps {
    /// Run `git` with the given args in `cwd`. Returns stdout on success.
    pub fn run(&self, cwd: &Path, args: &[&str]) -> Result<String, VmError> {
        let mut cmd = Command::new("git");
        cmd.current_dir(cwd);
        // Force a deterministic environment so commit SHAs are reproducible
        // when the manifest commit messages and author identities are stable.
        // We deliberately do not pass --date here because some operations
        // (e.g. clone) reject extra commit-only flags.
        cmd.env("GIT_AUTHOR_NAME", &self.author_name);
        cmd.env("GIT_AUTHOR_EMAIL", &self.author_email);
        cmd.env("GIT_AUTHOR_DATE", &self.committer_date);
        cmd.env("GIT_COMMITTER_NAME", &self.author_name);
        cmd.env("GIT_COMMITTER_EMAIL", &self.author_email);
        cmd.env("GIT_COMMITTER_DATE", &self.committer_date);
        // Stop git from picking up the user's signing config or hooks.
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("HOME", cwd);
        cmd.env("XDG_CONFIG_HOME", cwd);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("LANG", "C");
        // Clear ambient GIT_* env vars so we don't accidentally inherit
        // the caller's repo state (e.g. when `harn merge-captain mock` is
        // invoked from inside a git pre-push hook, which sets `GIT_DIR`
        // and friends to the outer repo's `.git`). Without this every
        // subsequent `git init --bare`, `git clone`, etc. would target
        // the outer repo and fail in confusing ways.
        for leaky in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_PREFIX",
            "GIT_COMMON_DIR",
            "GIT_INDEX_VERSION",
            "GIT_REFLOG_ACTION",
            "GIT_TRACE",
            "GIT_TRACE_PACKET",
            "GIT_TRACE_PERFORMANCE",
            "GIT_TRACE_SETUP",
        ] {
            cmd.env_remove(leaky);
        }
        cmd.args(args);
        let output = cmd.output().map_err(|error| {
            VmError::Runtime(format!(
                "failed to spawn `git {}` in {}: {error}",
                args.join(" "),
                cwd.display()
            ))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VmError::Runtime(format!(
                "`git {}` failed in {}: {}",
                args.join(" "),
                cwd.display(),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Initialize a bare remote at `path` with the given default branch.
    pub fn init_bare(&self, path: &Path, default_branch: &str) -> Result<(), VmError> {
        std::fs::create_dir_all(path).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create bare repo dir {}: {error}",
                path.display()
            ))
        })?;
        // Some older git versions don't support --initial-branch on bare init,
        // so we set HEAD explicitly afterwards.
        self.run(path, &["init", "--bare", "--quiet"])?;
        let head = format!("ref: refs/heads/{default_branch}\n");
        std::fs::write(path.join("HEAD"), head).map_err(|error| {
            VmError::Runtime(format!(
                "failed to set HEAD for {}: {error}",
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Clone `bare` into `working`.
    pub fn clone(&self, bare: &Path, working: &Path) -> Result<(), VmError> {
        if let Some(parent) = working.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to create clone parent {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let bare_str = bare.to_string_lossy();
        let working_str = working.to_string_lossy();
        let cwd = working
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.run(&cwd, &["clone", "--quiet", &bare_str, &working_str])?;
        // Also pin local user.* in the new clone so subsequent commits in
        // tests don't depend on the global gitconfig.
        self.run(working, &["config", "user.name", &self.author_name])?;
        self.run(working, &["config", "user.email", &self.author_email])?;
        Ok(())
    }

    /// Apply a file overlay (write contents) plus a delete list, then commit
    /// + push the current branch. Returns the new HEAD SHA.
    pub fn commit_overlay(
        &self,
        working: &Path,
        files_set: &BTreeMap<String, String>,
        files_delete: &[String],
        message: &str,
        push_target: Option<&str>,
    ) -> Result<String, VmError> {
        for (rel, contents) in files_set {
            let target = working.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    VmError::Runtime(format!(
                        "failed to create dir {}: {error}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::write(&target, contents).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to write file {}: {error}",
                    target.display()
                ))
            })?;
        }
        for rel in files_delete {
            let target = working.join(rel);
            if target.exists() {
                std::fs::remove_file(&target).map_err(|error| {
                    VmError::Runtime(format!(
                        "failed to remove file {}: {error}",
                        target.display()
                    ))
                })?;
            }
        }
        self.run(working, &["add", "--all"])?;
        // Allow empty in case the overlay was no-op (lets scenarios mark
        // intentional pivot commits without diffs).
        self.run(
            working,
            &["commit", "--allow-empty", "--quiet", "-m", message],
        )?;
        let sha = self.run(working, &["rev-parse", "HEAD"])?;
        let sha = sha.trim().to_string();
        if let Some(target) = push_target {
            self.run(working, &["push", "--quiet", "origin", target])?;
        }
        Ok(sha)
    }

    /// Force-rewrite a branch to a fresh single-commit history starting from
    /// the base branch, then `--force-with-lease` push to the bare remote.
    /// Used by `ForcePushAuthor` step actions.
    pub fn force_rewrite_branch(
        &self,
        working: &Path,
        branch: &str,
        base: &str,
        files_set: &BTreeMap<String, String>,
        files_delete: &[String],
        message: &str,
    ) -> Result<String, VmError> {
        // `--force` on checkout to discard whatever is currently on the branch.
        self.run(
            working,
            &["checkout", "-B", branch, &format!("origin/{base}")],
        )?;
        let sha = self.commit_overlay(working, files_set, files_delete, message, None)?;
        self.run(
            working,
            &["push", "--force-with-lease", "--quiet", "origin", branch],
        )?;
        Ok(sha)
    }

    /// Produce a real merge commit on the bare remote: checkout base, merge
    /// head with `--no-ff`, push.
    pub fn merge_branch(
        &self,
        working: &Path,
        head: &str,
        base: &str,
        message: &str,
    ) -> Result<String, VmError> {
        self.run(working, &["fetch", "--quiet", "origin"])?;
        self.run(working, &["checkout", base])?;
        self.run(working, &["pull", "--quiet", "--ff-only", "origin", base])?;
        self.run(
            working,
            &[
                "merge",
                "--no-ff",
                "--no-edit",
                "--quiet",
                "-m",
                message,
                &format!("origin/{head}"),
            ],
        )?;
        let sha = self.run(working, &["rev-parse", "HEAD"])?;
        let sha = sha.trim().to_string();
        self.run(working, &["push", "--quiet", "origin", base])?;
        Ok(sha)
    }

    pub fn checkout(&self, working: &Path, branch: &str) -> Result<(), VmError> {
        self.run(working, &["checkout", "--quiet", branch])?;
        Ok(())
    }

    pub fn create_branch(
        &self,
        working: &Path,
        branch: &str,
        from_ref: &str,
    ) -> Result<(), VmError> {
        self.run(working, &["checkout", "-b", branch, from_ref])?;
        Ok(())
    }

    pub fn rev_parse(&self, working: &Path, what: &str) -> Result<String, VmError> {
        let out = self.run(working, &["rev-parse", what])?;
        Ok(out.trim().to_string())
    }

    pub fn fetch(&self, working: &Path) -> Result<(), VmError> {
        self.run(working, &["fetch", "--quiet", "origin"])?;
        Ok(())
    }
}

/// `bare` represented as a `file://` URL — what the connector and a real
/// git client would consume.
pub fn bare_file_url(bare: &Path) -> String {
    let canonical = std::fs::canonicalize(bare).unwrap_or_else(|_| bare.to_path_buf());
    let mut url = String::from("file://");
    let canon_str = canonical.to_string_lossy();
    if !canon_str.starts_with('/') {
        url.push('/');
    }
    url.push_str(&canon_str);
    url
}
