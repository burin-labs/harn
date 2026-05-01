//! Scenario manifest types for the Merge Captain mock-repos playground (#1020).
//!
//! A manifest is checked-in YAML/JSON describing the *seed* state of a
//! playground: the repos, branches, pull requests, checks, and the
//! deterministic step actions an agent can use to advance the scenario.
//! `init_playground` consumes a manifest to materialize real on-disk git
//! repos plus a mutable `state.json`; the fake GitHub HTTP server reads
//! that state.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::value::VmError;

/// Manifest envelope `_type` field — used to refuse stray JSON files.
pub const SCENARIO_TYPE: &str = "merge_captain_playground_scenario";

/// Top-level scenario manifest.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub scenario: String,
    pub description: String,
    pub owner: String,
    pub repos: Vec<ScenarioRepo>,
    pub pull_requests: Vec<ScenarioPullRequest>,
    pub steps: Vec<ScenarioStep>,
}

/// A repo definition. The default branch always has a single seed commit;
/// each `branch` entry is a feature branch authored from that seed plus an
/// overlay of files.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioRepo {
    pub name: String,
    pub default_branch: String,
    /// File overlay applied to the default-branch seed commit. Path → content.
    pub files: BTreeMap<String, String>,
    /// Extra commits to apply on top of the default branch *before* feature
    /// branches are forked. Used to simulate "behind-base" scenarios where the
    /// feature branch was forked at an earlier point.
    pub default_branch_extra_commits: Vec<ScenarioCommit>,
    pub branches: Vec<ScenarioBranch>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioBranch {
    pub name: String,
    /// Optional base branch. Defaults to the repo's `default_branch`.
    pub base: Option<String>,
    /// Whether to fork *before* the default-branch extra commits. When
    /// true, the branch is created at the seed commit (i.e. behind base).
    pub fork_before_extra_commits: bool,
    /// File overlay applied as a single commit on top of the base.
    pub files_set: BTreeMap<String, String>,
    /// File deletions applied as part of the same commit.
    pub files_delete: Vec<String>,
    /// Optional commit message override.
    pub commit_message: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioCommit {
    pub message: String,
    pub files_set: BTreeMap<String, String>,
    pub files_delete: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioPullRequest {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `open`, `closed`, `merged`.
    pub state: String,
    pub head_branch: String,
    pub base_branch: String,
    pub user: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub checks: Vec<ScenarioCheck>,
    pub mergeable: Option<bool>,
    /// `clean`, `dirty`, `behind`, `blocked`, `unstable`.
    pub mergeable_state: String,
    /// `none`, `queued`, `merged`, `failed`.
    pub merge_queue_status: Option<String>,
    pub comments: Vec<ScenarioComment>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioCheck {
    pub name: String,
    /// `queued`, `in_progress`, `completed`.
    pub status: String,
    /// When `status == "completed"`: `success`, `failure`, `cancelled`,
    /// `timed_out`, `neutral`, `skipped`.
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioComment {
    pub user: String,
    pub body: String,
    pub created_at: Option<String>,
}

/// A named, declarative step the agent can run via `harn merge-captain mock
/// step <dir> <name>`. Steps are pure state mutations applied to
/// `state.json`; nothing inside a step touches the bare git remote.
/// (Force-push-by-author and similar git-native mutations are first-class
/// `ScenarioAction` variants so the underlying command is deterministic.)
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScenarioStep {
    pub name: String,
    pub description: String,
    pub actions: Vec<ScenarioAction>,
}

/// A single mutation applied to the playground state. Adding a variant here
/// is a backwards-compatible change because `serde(other)` rejects unknown
/// tags loudly so old binaries can't silently no-op a newer manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioAction {
    /// Update or insert a check run for a PR.
    SetCheck {
        repo: String,
        pr_number: u64,
        name: String,
        status: String,
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        details_url: Option<String>,
    },
    /// Append a new PR. The branch must already exist on the bare remote.
    AddPullRequest {
        #[serde(flatten)]
        pr: ScenarioPullRequest,
    },
    /// Mark a PR closed without merging.
    ClosePullRequest { repo: String, pr_number: u64 },
    /// Mark a PR merged. Produces a real merge commit on the bare remote
    /// (so subsequent rebases see a real diverged history).
    MergePullRequest {
        repo: String,
        pr_number: u64,
        #[serde(default)]
        merge_method: Option<String>,
    },
    /// Append a comment.
    AddComment {
        repo: String,
        pr_number: u64,
        user: String,
        body: String,
    },
    /// Set the PR's labels (replaces).
    SetLabels {
        repo: String,
        pr_number: u64,
        labels: Vec<String>,
    },
    /// Update the merge-queue status for a PR.
    SetMergeQueueStatus {
        repo: String,
        pr_number: u64,
        /// `none`, `queued`, `merged`, `failed`.
        status: String,
    },
    /// Simulate a force-push by the author: rewrite the head branch to a new
    /// snapshot of files. Updates the bare remote.
    ForcePushAuthor {
        repo: String,
        branch: String,
        files_set: BTreeMap<String, String>,
        #[serde(default)]
        files_delete: Vec<String>,
        #[serde(default)]
        commit_message: Option<String>,
    },
    /// Append a commit on the default branch of a repo (simulates someone
    /// else landing work, putting open PRs behind base).
    AdvanceBase {
        repo: String,
        #[serde(default)]
        files_set: BTreeMap<String, String>,
        #[serde(default)]
        files_delete: Vec<String>,
        #[serde(default)]
        commit_message: Option<String>,
    },
    /// Update mergeable / mergeable_state without git mutation.
    SetMergeability {
        repo: String,
        pr_number: u64,
        mergeable: Option<bool>,
        mergeable_state: String,
    },
    /// Advance the playground clock by N milliseconds. Steps that timestamp
    /// events use the playground clock so transcripts remain deterministic.
    AdvanceTimeMs { ms: u64 },
}

impl ScenarioManifest {
    /// Parse a manifest from JSON or YAML based on file extension.
    pub fn load(path: &Path) -> Result<Self, VmError> {
        let bytes = std::fs::read(path).map_err(|error| {
            VmError::Runtime(format!(
                "failed to read scenario manifest {}: {error}",
                path.display()
            ))
        })?;
        Self::parse(&bytes, path)
    }

    pub fn parse(bytes: &[u8], path: &Path) -> Result<Self, VmError> {
        let is_yaml = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
            .unwrap_or(false);
        let manifest: ScenarioManifest = if is_yaml {
            serde_yaml::from_slice(bytes).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse YAML scenario manifest {}: {error}",
                    path.display()
                ))
            })?
        } else {
            serde_json::from_slice(bytes).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse JSON scenario manifest {}: {error}",
                    path.display()
                ))
            })?
        };
        manifest.validate(path)?;
        Ok(manifest)
    }

    pub fn validate(&self, path: &Path) -> Result<(), VmError> {
        if self.type_name != SCENARIO_TYPE {
            return Err(VmError::Runtime(format!(
                "scenario manifest {} has _type {:?}, expected {SCENARIO_TYPE}",
                path.display(),
                self.type_name
            )));
        }
        if self.scenario.is_empty() {
            return Err(VmError::Runtime(format!(
                "scenario manifest {} is missing required field 'scenario'",
                path.display()
            )));
        }
        if self.owner.is_empty() {
            return Err(VmError::Runtime(format!(
                "scenario manifest {} is missing required field 'owner'",
                path.display()
            )));
        }
        if self.repos.is_empty() {
            return Err(VmError::Runtime(format!(
                "scenario manifest {} must declare at least one repo",
                path.display()
            )));
        }
        let mut repo_names = std::collections::HashSet::new();
        for repo in &self.repos {
            if repo.name.is_empty() {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} has a repo with no name",
                    path.display()
                )));
            }
            if !is_safe_segment(&repo.name) {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} repo name {:?} contains characters that aren't safe for filesystem paths (use [A-Za-z0-9._-])",
                    path.display(),
                    repo.name
                )));
            }
            if repo.default_branch.is_empty() {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} repo {} is missing default_branch",
                    path.display(),
                    repo.name
                )));
            }
            if !is_safe_ref(&repo.default_branch) {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} repo {} default_branch {:?} is not a valid git ref",
                    path.display(),
                    repo.name,
                    repo.default_branch
                )));
            }
            if !repo_names.insert(repo.name.clone()) {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} declares repo {} twice",
                    path.display(),
                    repo.name
                )));
            }
            let mut branch_names = std::collections::HashSet::new();
            branch_names.insert(repo.default_branch.clone());
            for branch in &repo.branches {
                if branch.name.is_empty() {
                    return Err(VmError::Runtime(format!(
                        "scenario manifest {} repo {} has a branch with no name",
                        path.display(),
                        repo.name
                    )));
                }
                if !is_safe_ref(&branch.name) {
                    return Err(VmError::Runtime(format!(
                        "scenario manifest {} repo {} branch {:?} is not a valid git ref",
                        path.display(),
                        repo.name,
                        branch.name
                    )));
                }
                if !branch_names.insert(branch.name.clone()) {
                    return Err(VmError::Runtime(format!(
                        "scenario manifest {} repo {} declares branch {} twice",
                        path.display(),
                        repo.name,
                        branch.name
                    )));
                }
            }
        }
        let repo_index: std::collections::HashMap<&str, &ScenarioRepo> =
            self.repos.iter().map(|r| (r.name.as_str(), r)).collect();
        let mut pr_keys = std::collections::HashSet::new();
        for pr in &self.pull_requests {
            let repo = repo_index.get(pr.repo.as_str()).ok_or_else(|| {
                VmError::Runtime(format!(
                    "scenario manifest {} pull_request #{} references unknown repo {}",
                    path.display(),
                    pr.number,
                    pr.repo
                ))
            })?;
            if !pr_keys.insert((pr.repo.clone(), pr.number)) {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} declares PR {}/{} twice",
                    path.display(),
                    pr.repo,
                    pr.number
                )));
            }
            if pr.head_branch.is_empty() {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} PR {}/{} is missing head_branch",
                    path.display(),
                    pr.repo,
                    pr.number
                )));
            }
            if pr.base_branch.is_empty() {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} PR {}/{} is missing base_branch",
                    path.display(),
                    pr.repo,
                    pr.number
                )));
            }
            // head_branch must exist in the repo (or equal default_branch).
            let head_exists = pr.head_branch == repo.default_branch
                || repo.branches.iter().any(|b| b.name == pr.head_branch);
            if !head_exists {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} PR {}/{} head_branch {} is not declared on repo {}",
                    path.display(),
                    pr.repo,
                    pr.number,
                    pr.head_branch,
                    pr.repo
                )));
            }
        }
        let mut step_names = std::collections::HashSet::new();
        for step in &self.steps {
            if step.name.is_empty() {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} has an unnamed step",
                    path.display()
                )));
            }
            if !step_names.insert(step.name.clone()) {
                return Err(VmError::Runtime(format!(
                    "scenario manifest {} declares step {} twice",
                    path.display(),
                    step.name
                )));
            }
        }
        Ok(())
    }
}

/// Repos materialize as a directory under `<playground>/{remotes,working}/`,
/// so reject anything with path-separator-like characters or leading dots.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Git refs allow `/` (e.g. `feature/foo`) but disallow control chars,
/// `..`, leading/trailing slashes, and `@{`. We use a conservative subset
/// rather than re-implementing `git check-ref-format`.
fn is_safe_ref(s: &str) -> bool {
    if s.is_empty() || s.starts_with('/') || s.ends_with('/') || s.contains("..") {
        return false;
    }
    s.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' || c == '+'
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn json(s: &str) -> ScenarioManifest {
        ScenarioManifest::parse(s.as_bytes(), &PathBuf::from("test.json")).unwrap()
    }

    #[test]
    fn parses_minimal_manifest() {
        let m = json(
            r#"{
  "_type": "merge_captain_playground_scenario",
  "scenario": "x",
  "owner": "burin-labs",
  "repos": [{"name": "alpha", "default_branch": "main"}]
}"#,
        );
        assert_eq!(m.scenario, "x");
        assert_eq!(m.repos.len(), 1);
    }

    #[test]
    fn rejects_wrong_type() {
        let err = ScenarioManifest::parse(
            br#"{"_type": "wrong", "scenario": "x", "owner": "burin-labs", "repos": [{"name": "a", "default_branch": "main"}]}"#,
            &PathBuf::from("test.json"),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("_type"));
    }

    #[test]
    fn rejects_unknown_pr_repo() {
        let err = ScenarioManifest::parse(
            br#"{"_type": "merge_captain_playground_scenario", "scenario": "x", "owner": "burin-labs",
              "repos": [{"name": "alpha", "default_branch": "main"}],
              "pull_requests": [{"repo": "ghost", "number": 1, "head_branch": "main", "base_branch": "main", "mergeable_state": "clean"}]}"#,
            &PathBuf::from("test.json"),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("unknown repo"));
    }

    #[test]
    fn rejects_path_traversal_repo_name() {
        let err = ScenarioManifest::parse(
            br#"{"_type": "merge_captain_playground_scenario", "scenario": "x", "owner": "burin-labs",
              "repos": [{"name": "../../etc", "default_branch": "main"}]}"#,
            &PathBuf::from("test.json"),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("safe for filesystem paths"));
    }

    #[test]
    fn rejects_invalid_branch_ref() {
        let err = ScenarioManifest::parse(
            br#"{"_type": "merge_captain_playground_scenario", "scenario": "x", "owner": "burin-labs",
              "repos": [{"name": "alpha", "default_branch": "main",
                         "branches": [{"name": "..feature"}]}]}"#,
            &PathBuf::from("test.json"),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("not a valid git ref"));
    }

    #[test]
    fn rejects_duplicate_step_name() {
        let err = ScenarioManifest::parse(
            br#"{"_type": "merge_captain_playground_scenario", "scenario": "x", "owner": "burin-labs",
              "repos": [{"name": "alpha", "default_branch": "main"}],
              "steps": [{"name": "go"}, {"name": "go"}]}"#,
            &PathBuf::from("test.json"),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("step"));
    }

    #[test]
    fn parses_all_action_kinds() {
        let m = json(
            r#"{
  "_type": "merge_captain_playground_scenario",
  "scenario": "x",
  "owner": "burin-labs",
  "repos": [{"name": "alpha", "default_branch": "main",
             "branches": [{"name": "feature/a", "files_set": {"a.txt": "1"}}]}],
  "pull_requests": [{"repo": "alpha", "number": 1, "head_branch": "feature/a", "base_branch": "main", "mergeable_state": "clean"}],
  "steps": [
    {"name": "all", "actions": [
      {"kind": "set_check", "repo": "alpha", "pr_number": 1, "name": "ci", "status": "completed", "conclusion": "success"},
      {"kind": "close_pull_request", "repo": "alpha", "pr_number": 1},
      {"kind": "merge_pull_request", "repo": "alpha", "pr_number": 1},
      {"kind": "add_comment", "repo": "alpha", "pr_number": 1, "user": "alice", "body": "lgtm"},
      {"kind": "set_labels", "repo": "alpha", "pr_number": 1, "labels": ["ready"]},
      {"kind": "set_merge_queue_status", "repo": "alpha", "pr_number": 1, "status": "queued"},
      {"kind": "force_push_author", "repo": "alpha", "branch": "feature/a", "files_set": {"a.txt": "2"}},
      {"kind": "advance_base", "repo": "alpha", "files_set": {"main.txt": "1"}},
      {"kind": "set_mergeability", "repo": "alpha", "pr_number": 1, "mergeable": true, "mergeable_state": "behind"},
      {"kind": "advance_time_ms", "ms": 60000}
    ]}
  ]
}"#,
        );
        let actions = &m.steps[0].actions;
        assert_eq!(actions.len(), 10);
    }
}
