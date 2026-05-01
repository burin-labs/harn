//! Mutable playground state persisted to `<dir>/state.json`. The fake
//! GitHub HTTP server (#1020) reads/writes this file on every request,
//! which is what gives the playground its "real on-disk sandbox"
//! feel — every captain sweep observes the live state, and every
//! `mock step` produces a durable diff that subsequent sweeps see.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::value::VmError;

use super::manifest::{
    ScenarioCheck, ScenarioComment, ScenarioManifest, ScenarioPullRequest, ScenarioStep,
};

pub const STATE_TYPE: &str = "merge_captain_playground_state";
pub const PLAYGROUND_TYPE: &str = "merge_captain_playground";

/// A snapshot of the playground's mutable state.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlaygroundState {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    /// Owner used in API URLs; cloned from the manifest.
    pub owner: String,
    /// Logical scenario name (provenance only).
    pub scenario: String,
    /// Monotonic step counter — incremented every time `apply_action` runs.
    pub step_count: u64,
    /// Playground clock, in milliseconds since UNIX epoch. Defaults to a
    /// stable seed (2026-01-01) so transcripts are byte-stable across runs.
    pub now_ms: i64,
    /// Per-repo state.
    pub repos: BTreeMap<String, PlaygroundRepoState>,
    /// Global PR list keyed by `<repo>#<number>`.
    pub pull_requests: BTreeMap<String, PlaygroundPullRequest>,
    /// History of applied actions (most recent last) — used by `status`
    /// and by the synthetic transcript builder to record sweep activity.
    pub history: Vec<PlaygroundHistoryEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlaygroundRepoState {
    pub name: String,
    pub default_branch: String,
    /// Bare-remote-relative branches that exist (kept in sync with the
    /// remote in `init` and `step`).
    pub branches: Vec<String>,
    /// Resolved file:// URL of the bare remote (without trailing slash).
    pub remote_url: String,
    /// Path of the working clone (relative to the playground root).
    pub working_path: String,
    /// Path of the bare remote (relative to the playground root).
    pub remote_path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PlaygroundPullRequest {
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
    pub mergeable_state: String,
    pub merge_queue_status: Option<String>,
    pub comments: Vec<ScenarioComment>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    /// Latest commit SHA on the head branch (set by `init` and updated
    /// after force-push / advance-base / merge).
    pub head_sha: Option<String>,
}

impl PlaygroundPullRequest {
    pub fn key(&self) -> String {
        Self::compose_key(&self.repo, self.number)
    }

    pub fn compose_key(repo: &str, number: u64) -> String {
        format!("{repo}#{number}")
    }
}

/// One entry in the playground history log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaygroundHistoryEntry {
    /// Monotonic id starting at 1.
    pub seq: u64,
    /// `init`, `step:<name>`, `action:<kind>`.
    pub source: String,
    /// JSON description of the action (for replay/audit).
    pub detail: serde_json::Value,
    /// Playground-clock millis at the time of the action.
    pub at_ms: i64,
}

impl PlaygroundState {
    pub fn from_manifest(manifest: &ScenarioManifest) -> Self {
        let mut state = PlaygroundState {
            type_name: STATE_TYPE.to_string(),
            version: 1,
            owner: manifest.owner.clone(),
            scenario: manifest.scenario.clone(),
            step_count: 0,
            now_ms: 1_767_225_600_000, // 2026-01-01T00:00:00Z, deterministic seed.
            ..Default::default()
        };
        for repo in &manifest.repos {
            let mut branches = vec![repo.default_branch.clone()];
            for branch in &repo.branches {
                branches.push(branch.name.clone());
            }
            state.repos.insert(
                repo.name.clone(),
                PlaygroundRepoState {
                    name: repo.name.clone(),
                    default_branch: repo.default_branch.clone(),
                    branches,
                    remote_url: String::new(),
                    working_path: format!("working/{}", repo.name),
                    remote_path: format!("remotes/{}.git", repo.name),
                },
            );
        }
        for pr in &manifest.pull_requests {
            let pr_state = PlaygroundPullRequest::from_manifest_pr(pr);
            state.pull_requests.insert(pr_state.key(), pr_state);
        }
        state
    }

    pub fn step_index(&self, name: &str, manifest: &ScenarioManifest) -> Option<usize> {
        manifest.steps.iter().position(|step| step.name == name)
    }

    pub fn list_steps<'a>(&self, manifest: &'a ScenarioManifest) -> Vec<&'a ScenarioStep> {
        manifest.steps.iter().collect()
    }

    pub fn record(&mut self, source: &str, detail: serde_json::Value) {
        self.step_count += 1;
        self.history.push(PlaygroundHistoryEntry {
            seq: self.step_count,
            source: source.to_string(),
            detail,
            at_ms: self.now_ms,
        });
    }

    pub fn save(&self, dir: &Path) -> Result<(), VmError> {
        let path = state_path(dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to create playground dir {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            VmError::Runtime(format!("failed to serialize playground state: {error}"))
        })?;
        bytes.push(b'\n');
        std::fs::write(&path, bytes).map_err(|error| {
            VmError::Runtime(format!(
                "failed to write playground state {}: {error}",
                path.display()
            ))
        })
    }

    pub fn load(dir: &Path) -> Result<Self, VmError> {
        let path = state_path(dir);
        let bytes = std::fs::read(&path).map_err(|error| {
            VmError::Runtime(format!(
                "failed to read playground state {}: {error}",
                path.display()
            ))
        })?;
        let state: PlaygroundState = serde_json::from_slice(&bytes).map_err(|error| {
            VmError::Runtime(format!(
                "failed to parse playground state {}: {error}",
                path.display()
            ))
        })?;
        if state.type_name != STATE_TYPE {
            return Err(VmError::Runtime(format!(
                "playground state {} has _type {:?}, expected {STATE_TYPE}",
                path.display(),
                state.type_name
            )));
        }
        Ok(state)
    }
}

impl PlaygroundPullRequest {
    pub fn from_manifest_pr(pr: &ScenarioPullRequest) -> Self {
        let state = if pr.state.is_empty() {
            "open".to_string()
        } else {
            pr.state.clone()
        };
        let mergeable_state = if pr.mergeable_state.is_empty() {
            "clean".to_string()
        } else {
            pr.mergeable_state.clone()
        };
        PlaygroundPullRequest {
            repo: pr.repo.clone(),
            number: pr.number,
            title: pr.title.clone(),
            body: pr.body.clone(),
            state,
            head_branch: pr.head_branch.clone(),
            base_branch: pr.base_branch.clone(),
            user: if pr.user.is_empty() {
                "playground-author".to_string()
            } else {
                pr.user.clone()
            },
            draft: pr.draft,
            labels: pr.labels.clone(),
            checks: pr.checks.clone(),
            mergeable: pr.mergeable,
            mergeable_state,
            merge_queue_status: pr.merge_queue_status.clone(),
            comments: pr.comments.clone(),
            merged_at: None,
            closed_at: None,
            head_sha: None,
        }
    }
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

pub fn playground_marker_path(dir: &Path) -> PathBuf {
    dir.join("playground.json")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaygroundMarker {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub scenario: String,
    pub created_at_ms: i64,
}

impl PlaygroundMarker {
    pub fn new(scenario: &str, created_at_ms: i64) -> Self {
        PlaygroundMarker {
            type_name: PLAYGROUND_TYPE.to_string(),
            version: 1,
            scenario: scenario.to_string(),
            created_at_ms,
        }
    }
}
