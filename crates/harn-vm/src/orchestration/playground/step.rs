//! Step actions — the levers a brute-forcing agent pulls between captain
//! sweeps in a Merge Captain mock-repos playground (#1020).
//!
//! Step actions are split into two flavors:
//!
//! * **State-only**: mutations that only touch `state.json` (e.g.
//!   `set_check`, `set_labels`, `add_comment`).
//! * **Git-native**: mutations that produce real commits on the bare
//!   remote (e.g. `merge_pull_request`, `force_push_author`,
//!   `advance_base`). The captain's subsequent rebase/fetch sees the
//!   real diverged history.

use std::path::Path;

use serde_json::json;

use crate::value::VmError;

use super::git::GitOps;
use super::manifest::{ScenarioAction, ScenarioManifest, ScenarioStep};
use super::state::{PlaygroundPullRequest, PlaygroundState};

pub struct StepReport {
    /// Number of state mutations applied (may be more than 1 per step).
    pub actions_applied: usize,
    /// Names of PRs whose state was updated. Useful for `mock status`.
    pub prs_touched: Vec<String>,
    /// Optional human-readable summary lines.
    pub summary: Vec<String>,
}

pub fn run_named_step(
    dir: &Path,
    state: &mut PlaygroundState,
    manifest: &ScenarioManifest,
    step_name: &str,
) -> Result<StepReport, VmError> {
    let step = manifest
        .steps
        .iter()
        .find(|s| s.name == step_name)
        .ok_or_else(|| {
            let available: Vec<String> = manifest.steps.iter().map(|s| s.name.clone()).collect();
            VmError::Runtime(format!(
                "scenario {} has no step named {step_name:?} (available: {})",
                manifest.scenario,
                if available.is_empty() {
                    "<none>".to_string()
                } else {
                    available.join(", ")
                }
            ))
        })?
        .clone();
    apply_step(dir, state, manifest, &step)
}

pub fn apply_step(
    dir: &Path,
    state: &mut PlaygroundState,
    manifest: &ScenarioManifest,
    step: &ScenarioStep,
) -> Result<StepReport, VmError> {
    let mut report = StepReport {
        actions_applied: 0,
        prs_touched: Vec::new(),
        summary: Vec::new(),
    };
    for action in &step.actions {
        let summary = apply_action(dir, state, manifest, action)?;
        report.actions_applied += 1;
        if let Some((pr, line)) = summary {
            if !report.prs_touched.contains(&pr) {
                report.prs_touched.push(pr);
            }
            report.summary.push(line);
        }
    }
    state.record(
        &format!("step:{}", step.name),
        json!({"actions": step.actions.len()}),
    );
    Ok(report)
}

/// Apply a single ad-hoc action without going through a named step. Used by
/// the `mock step --action <json>` CLI fallback for one-off mutations.
pub fn apply_one_action(
    dir: &Path,
    state: &mut PlaygroundState,
    manifest: &ScenarioManifest,
    action: &ScenarioAction,
) -> Result<StepReport, VmError> {
    let summary = apply_action(dir, state, manifest, action)?;
    state.record(
        &format!("action:{}", action_kind(action)),
        serde_json::to_value(action).unwrap_or(serde_json::Value::Null),
    );
    let mut report = StepReport {
        actions_applied: 1,
        prs_touched: Vec::new(),
        summary: Vec::new(),
    };
    if let Some((pr, line)) = summary {
        report.prs_touched.push(pr);
        report.summary.push(line);
    }
    Ok(report)
}

fn apply_action(
    dir: &Path,
    state: &mut PlaygroundState,
    manifest: &ScenarioManifest,
    action: &ScenarioAction,
) -> Result<Option<(String, String)>, VmError> {
    let git = GitOps::default();
    match action {
        ScenarioAction::SetCheck {
            repo,
            pr_number,
            name,
            status,
            conclusion,
            details_url,
        } => {
            let now_ms = state.now_ms;
            let pr = require_pr_mut(state, repo, *pr_number)?;
            let existing = pr.checks.iter_mut().find(|c| &c.name == name);
            let conclusion_value = conclusion.clone();
            let started = Some(format_clock(now_ms));
            let completed = if status == "completed" {
                Some(format_clock(now_ms + 1))
            } else {
                None
            };
            match existing {
                Some(check) => {
                    check.status = status.clone();
                    check.conclusion = conclusion_value;
                    if check.started_at.is_none() {
                        check.started_at = started;
                    }
                    check.completed_at = completed;
                    check.details_url = details_url.clone();
                }
                None => {
                    pr.checks.push(super::manifest::ScenarioCheck {
                        name: name.clone(),
                        status: status.clone(),
                        conclusion: conclusion_value,
                        details_url: details_url.clone(),
                        started_at: started,
                        completed_at: completed,
                    });
                }
            }
            Ok(Some((
                pr.key(),
                format!("set check {name}={status} on {repo}#{pr_number}"),
            )))
        }
        ScenarioAction::AddPullRequest { pr } => {
            if !state.repos.contains_key(&pr.repo) {
                return Err(VmError::Runtime(format!("unknown repo {}", pr.repo)));
            }
            let pr_state = PlaygroundPullRequest::from_manifest_pr(pr);
            let key = pr_state.key();
            if state.pull_requests.contains_key(&key) {
                return Err(VmError::Runtime(format!("PR {key} already exists")));
            }
            // Resolve head_sha if the head branch exists on the remote.
            let working = working_path(dir, &pr.repo);
            let head_sha = git
                .rev_parse(&working, &format!("origin/{}", pr.head_branch))
                .ok();
            let mut pr_state = pr_state;
            pr_state.head_sha = head_sha;
            state.pull_requests.insert(key.clone(), pr_state);
            Ok(Some((key, format!("opened PR {}#{}", pr.repo, pr.number))))
        }
        ScenarioAction::ClosePullRequest { repo, pr_number } => {
            let now_ms = state.now_ms;
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.state = "closed".to_string();
            pr.closed_at = Some(format_clock(now_ms));
            Ok(Some((pr.key(), format!("closed PR {repo}#{pr_number}"))))
        }
        ScenarioAction::MergePullRequest {
            repo,
            pr_number,
            merge_method,
        } => {
            let (head_branch, base_branch, title) = {
                let pr = require_pr(state, repo, *pr_number)?;
                (
                    pr.head_branch.clone(),
                    pr.base_branch.clone(),
                    pr.title.clone(),
                )
            };
            let working = working_path(dir, repo);
            git.fetch(&working)?;
            let merge_message =
                format!("Merge pull request #{pr_number} from {repo}/{head_branch}\n\n{title}");
            let merge_sha =
                git.merge_branch(&working, &head_branch, &base_branch, &merge_message)?;
            let now_ms = state.now_ms;
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.state = "merged".to_string();
            pr.merged_at = Some(format_clock(now_ms));
            pr.head_sha = Some(merge_sha);
            pr.mergeable_state = "clean".to_string();
            pr.mergeable = Some(true);
            pr.merge_queue_status = match merge_method.as_deref() {
                Some(method) if method.eq_ignore_ascii_case("queue") => Some("merged".to_string()),
                _ => pr.merge_queue_status.clone(),
            };
            Ok(Some((
                pr.key(),
                format!("merged PR {repo}#{pr_number} into {base_branch}"),
            )))
        }
        ScenarioAction::AddComment {
            repo,
            pr_number,
            user,
            body,
        } => {
            let now_ms = state.now_ms;
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.comments.push(super::manifest::ScenarioComment {
                user: user.clone(),
                body: body.clone(),
                created_at: Some(format_clock(now_ms)),
            });
            Ok(Some((
                pr.key(),
                format!("comment by {user} on {repo}#{pr_number}"),
            )))
        }
        ScenarioAction::SetLabels {
            repo,
            pr_number,
            labels,
        } => {
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.labels = labels.clone();
            Ok(Some((
                pr.key(),
                format!("labels={labels:?} on {repo}#{pr_number}"),
            )))
        }
        ScenarioAction::SetMergeQueueStatus {
            repo,
            pr_number,
            status,
        } => {
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.merge_queue_status = Some(status.clone());
            Ok(Some((
                pr.key(),
                format!("merge_queue_status={status} on {repo}#{pr_number}"),
            )))
        }
        ScenarioAction::ForcePushAuthor {
            repo,
            branch,
            files_set,
            files_delete,
            commit_message,
        } => {
            let working = working_path(dir, repo);
            // Refresh local refs so origin/<base> is current before
            // we reset the head branch onto it.
            git.fetch(&working)?;
            let base = manifest_base_for_branch(manifest, repo, branch);
            let message = commit_message
                .clone()
                .unwrap_or_else(|| format!("Force-pushed rewrite on {branch}"));
            let new_sha = git.force_rewrite_branch(
                &working,
                branch,
                &base,
                files_set,
                files_delete,
                &message,
            )?;
            // Update head_sha on any PRs that point at this branch.
            let mut keys = Vec::new();
            for pr in state
                .pull_requests
                .values_mut()
                .filter(|pr| pr.repo == *repo && pr.head_branch == *branch)
            {
                pr.head_sha = Some(new_sha.clone());
                pr.mergeable_state = "unknown".to_string();
                pr.mergeable = None;
                keys.push(pr.key());
            }
            let summary_key = keys.first().cloned().unwrap_or_else(|| repo.clone());
            Ok(Some((
                summary_key,
                format!("force-push by author on {repo}/{branch}"),
            )))
        }
        ScenarioAction::AdvanceBase {
            repo,
            files_set,
            files_delete,
            commit_message,
        } => {
            let working = working_path(dir, repo);
            git.fetch(&working)?;
            let default_branch = state
                .repos
                .get(repo)
                .map(|r| r.default_branch.clone())
                .ok_or_else(|| VmError::Runtime(format!("unknown repo {repo}")))?;
            git.checkout(&working, &default_branch)?;
            git.run(
                &working,
                &["pull", "--quiet", "--ff-only", "origin", &default_branch],
            )?;
            let message = commit_message
                .clone()
                .unwrap_or_else(|| format!("Advance {default_branch}"));
            let _new_sha = git.commit_overlay(
                &working,
                files_set,
                files_delete,
                &message,
                Some(&default_branch),
            )?;
            // Mark all open PRs with this base as `behind`.
            let mut prs_touched = Vec::new();
            for pr in state.pull_requests.values_mut().filter(|pr| {
                pr.repo == *repo && pr.base_branch == default_branch && pr.state == "open"
            }) {
                pr.mergeable_state = "behind".to_string();
                pr.mergeable = Some(false);
                prs_touched.push(pr.key());
            }
            Ok(Some((
                prs_touched.first().cloned().unwrap_or_else(|| repo.clone()),
                format!("advanced base {repo}/{default_branch}"),
            )))
        }
        ScenarioAction::SetMergeability {
            repo,
            pr_number,
            mergeable,
            mergeable_state,
        } => {
            let pr = require_pr_mut(state, repo, *pr_number)?;
            pr.mergeable = *mergeable;
            pr.mergeable_state = mergeable_state.clone();
            Ok(Some((
                pr.key(),
                format!("mergeable_state={mergeable_state} on {repo}#{pr_number}"),
            )))
        }
        ScenarioAction::AdvanceTimeMs { ms } => {
            state.now_ms = state.now_ms.saturating_add(*ms as i64);
            Ok(None)
        }
    }
}

fn require_pr_mut<'a>(
    state: &'a mut PlaygroundState,
    repo: &str,
    number: u64,
) -> Result<&'a mut PlaygroundPullRequest, VmError> {
    let key = PlaygroundPullRequest::compose_key(repo, number);
    state
        .pull_requests
        .get_mut(&key)
        .ok_or_else(|| VmError::Runtime(format!("unknown PR {key}")))
}

fn require_pr<'a>(
    state: &'a PlaygroundState,
    repo: &str,
    number: u64,
) -> Result<&'a PlaygroundPullRequest, VmError> {
    let key = PlaygroundPullRequest::compose_key(repo, number);
    state
        .pull_requests
        .get(&key)
        .ok_or_else(|| VmError::Runtime(format!("unknown PR {key}")))
}

fn working_path(dir: &Path, repo: &str) -> std::path::PathBuf {
    dir.join("working").join(repo)
}

fn manifest_base_for_branch(manifest: &ScenarioManifest, repo: &str, branch: &str) -> String {
    if let Some(repo_def) = manifest.repos.iter().find(|r| r.name == repo) {
        if let Some(b) = repo_def.branches.iter().find(|b| b.name == branch) {
            return b
                .base
                .clone()
                .unwrap_or_else(|| repo_def.default_branch.clone());
        }
        return repo_def.default_branch.clone();
    }
    "main".to_string()
}

fn action_kind(action: &ScenarioAction) -> &'static str {
    match action {
        ScenarioAction::SetCheck { .. } => "set_check",
        ScenarioAction::AddPullRequest { .. } => "add_pull_request",
        ScenarioAction::ClosePullRequest { .. } => "close_pull_request",
        ScenarioAction::MergePullRequest { .. } => "merge_pull_request",
        ScenarioAction::AddComment { .. } => "add_comment",
        ScenarioAction::SetLabels { .. } => "set_labels",
        ScenarioAction::SetMergeQueueStatus { .. } => "set_merge_queue_status",
        ScenarioAction::ForcePushAuthor { .. } => "force_push_author",
        ScenarioAction::AdvanceBase { .. } => "advance_base",
        ScenarioAction::SetMergeability { .. } => "set_mergeability",
        ScenarioAction::AdvanceTimeMs { .. } => "advance_time_ms",
    }
}

fn format_clock(now_ms: i64) -> String {
    use chrono::TimeZone;
    let utc = chrono::Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_else(chrono::Utc::now);
    utc.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
