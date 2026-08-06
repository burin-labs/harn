//! Pure fake-GitHub HTTP request handlers for the Merge Captain mock-repos
//! playground (#1020). The CLI binds these to an axum `Router`; tests can
//! call them directly to assert per-endpoint behavior without TCP.
//!
//! The endpoint surface is the strict subset the issue calls out:
//! `pulls`, `checks`, `actions/runs/.../logs`, `merge_queue`, `issues`,
//! `comments`, `pulls/.../merge`. Every response is sourced from the
//! mutable `PlaygroundState`; mutating endpoints update the state in
//! place and return the updated representation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::manifest::ScenarioComment;
use super::state::{PlaygroundPullRequest, PlaygroundState};

/// Response envelope for any handler.
#[derive(Clone, Debug, Serialize)]
pub struct FakeResponse {
    pub status: u16,
    pub body: Value,
}

impl FakeResponse {
    pub fn ok(body: Value) -> Self {
        FakeResponse { status: 200, body }
    }
    pub fn created(body: Value) -> Self {
        FakeResponse { status: 201, body }
    }
    pub fn not_found(message: &str) -> Self {
        FakeResponse {
            status: 404,
            body: json!({"message": message, "documentation_url": ""}),
        }
    }
    pub fn unprocessable(message: &str) -> Self {
        FakeResponse {
            status: 422,
            body: json!({"message": message, "documentation_url": ""}),
        }
    }
    pub fn bad_request(message: &str) -> Self {
        FakeResponse {
            status: 400,
            body: json!({"message": message, "documentation_url": ""}),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListPullsQuery {
    pub state: Option<String>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub per_page: Option<u32>,
}

pub fn list_pulls(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    query: &ListPullsQuery,
) -> FakeResponse {
    if state.owner != owner {
        return FakeResponse::not_found(&format!("unknown owner {owner}"));
    }
    if !state.repos.contains_key(repo) {
        return FakeResponse::not_found(&format!("unknown repo {owner}/{repo}"));
    }
    let want_state = query
        .state
        .clone()
        .unwrap_or_else(|| "open".to_string())
        .to_lowercase();
    let mut prs: Vec<&PlaygroundPullRequest> = state
        .pull_requests
        .values()
        .filter(|pr| pr.repo == repo)
        .filter(|pr| match want_state.as_str() {
            "all" => true,
            other => pr.state.eq_ignore_ascii_case(other),
        })
        .filter(|pr| {
            query
                .base
                .as_ref()
                .map(|b| pr.base_branch == *b)
                .unwrap_or(true)
        })
        .filter(|pr| {
            query
                .head
                .as_ref()
                .map(|h| {
                    let parts: Vec<&str> = h.split(':').collect();
                    let branch = parts.last().copied().unwrap_or("");
                    pr.head_branch == branch
                })
                .unwrap_or(true)
        })
        .collect();
    prs.sort_by_key(|pr| pr.number);
    let body = Value::Array(prs.iter().map(|pr| pr_to_v3(state, pr)).collect());
    FakeResponse::ok(body)
}

pub fn get_pull(state: &PlaygroundState, owner: &str, repo: &str, number: u64) -> FakeResponse {
    let Some(pr) = pr_lookup(state, owner, repo, number) else {
        return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
    };
    FakeResponse::ok(pr_to_v3(state, pr))
}

pub fn list_pull_files(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
) -> FakeResponse {
    let Some(pr) = pr_lookup(state, owner, repo, number) else {
        return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
    };
    // The fake server doesn't compute real diffs — it returns a stable stub
    // describing the head_branch/base_branch pair so the consumer can decide
    // whether to fetch the working tree itself.
    FakeResponse::ok(json!([
        {
            "filename": format!("{}-vs-{}.summary", pr.head_branch, pr.base_branch),
            "status": "modified",
            "additions": 1,
            "deletions": 0,
            "changes": 1,
            "patch": ""
        }
    ]))
}

pub fn list_check_runs(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    sha_or_ref: &str,
) -> FakeResponse {
    if state.owner != owner || !state.repos.contains_key(repo) {
        return FakeResponse::not_found("unknown repo");
    }
    // Find a PR whose head_sha or head_branch matches the ref/sha.
    let pr = state.pull_requests.values().find(|pr| {
        pr.repo == repo
            && (pr
                .head_sha
                .as_deref()
                .map(|sha| sha == sha_or_ref)
                .unwrap_or(false)
                || pr.head_branch == sha_or_ref)
    });
    let runs: Vec<Value> = match pr {
        Some(pr) => pr
            .checks
            .iter()
            .enumerate()
            .map(|(idx, check)| {
                json!({
                    "id": (pr.number * 1000 + idx as u64),
                    "name": check.name,
                    "status": check.status,
                    "conclusion": check.conclusion,
                    "head_sha": pr.head_sha,
                    "details_url": check.details_url,
                    "started_at": check.started_at,
                    "completed_at": check.completed_at,
                })
            })
            .collect(),
        None => Vec::new(),
    };
    FakeResponse::ok(json!({"total_count": runs.len(), "check_runs": runs}))
}

pub fn workflow_run_logs(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> FakeResponse {
    // We don't keep real logs; just return a deterministic synthetic blob
    // keyed by run_id so connector code that streams logs has something
    // to assert on.
    if state.owner != owner || !state.repos.contains_key(repo) {
        return FakeResponse::not_found("unknown repo");
    }
    let body = json!({
        "run_id": run_id,
        "log_lines": [
            format!("[{owner}/{repo}#{run_id}] starting"),
            format!("[{owner}/{repo}#{run_id}] step 1 / 1 succeeded"),
            format!("[{owner}/{repo}#{run_id}] finished")
        ]
    });
    FakeResponse::ok(body)
}

pub fn list_issue_comments(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
) -> FakeResponse {
    let Some(pr) = pr_lookup(state, owner, repo, number) else {
        return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
    };
    let body = Value::Array(
        pr.comments
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                json!({
                    "id": pr.number * 1000 + idx as u64,
                    "body": c.body,
                    "user": {"login": c.user},
                    "created_at": c.created_at
                })
            })
            .collect(),
    );
    FakeResponse::ok(body)
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateCommentBody {
    pub body: String,
    #[serde(default)]
    pub user: Option<String>,
}

pub fn create_issue_comment(
    state: &mut PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
    payload: CreateCommentBody,
) -> FakeResponse {
    let pr_key = PlaygroundPullRequest::compose_key(repo, number);
    if state.owner != owner {
        return FakeResponse::not_found("unknown owner");
    }
    let now_ms = state.now_ms;
    let id;
    let user = payload
        .user
        .clone()
        .unwrap_or_else(|| "playground-bot".to_string());
    let now = format_now(now_ms);
    {
        let Some(pr) = state.pull_requests.get_mut(&pr_key) else {
            return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
        };
        pr.comments.push(ScenarioComment {
            user: user.clone(),
            body: payload.body.clone(),
            created_at: Some(now.clone()),
        });
        id = pr.number * 1000 + (pr.comments.len() as u64 - 1);
    }
    state.record(
        "fake_server:create_issue_comment",
        json!({"repo": repo, "number": number, "user": user}),
    );
    FakeResponse::created(json!({
        "id": id,
        "body": payload.body,
        "user": {"login": user},
        "created_at": now
    }))
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct UpdatePullBody {
    pub state: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub labels: Option<Vec<String>>,
}

pub fn patch_pull(
    state: &mut PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
    payload: UpdatePullBody,
) -> FakeResponse {
    let pr_key = PlaygroundPullRequest::compose_key(repo, number);
    if state.owner != owner {
        return FakeResponse::not_found("unknown owner");
    }
    let pr_clone = {
        let Some(pr) = state.pull_requests.get_mut(&pr_key) else {
            return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
        };
        if let Some(s) = payload.state {
            pr.state = s;
        }
        if let Some(t) = payload.title {
            pr.title = t;
        }
        if let Some(b) = payload.body {
            pr.body = b;
        }
        if let Some(base) = payload.base {
            pr.base_branch = base;
        }
        if let Some(labels) = payload.labels {
            pr.labels = labels;
        }
        pr.clone()
    };
    state.record(
        "fake_server:patch_pull",
        json!({"repo": repo, "number": number}),
    );
    FakeResponse::ok(pr_to_v3(state, &pr_clone))
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MergePullBody {
    pub merge_method: Option<String>,
    pub commit_title: Option<String>,
    pub commit_message: Option<String>,
}

/// `PUT /repos/:owner/:repo/pulls/:number/merge`. Updates the PR state to
/// `merged` but does NOT produce a real merge commit on the bare remote —
/// the connector or `mock step --action merge_pull_request` is responsible
/// for the git mutation. Mirrors GitHub's API which performs the merge
/// server-side; here it's a pure state update.
pub fn merge_pull(
    state: &mut PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
    payload: MergePullBody,
) -> FakeResponse {
    let pr_key = PlaygroundPullRequest::compose_key(repo, number);
    if state.owner != owner {
        return FakeResponse::not_found("unknown owner");
    }
    let now_ms = state.now_ms;
    let _method = payload.merge_method.unwrap_or_else(|| "merge".to_string());
    let head_sha = {
        let Some(pr) = state.pull_requests.get_mut(&pr_key) else {
            return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
        };
        if pr.state != "open" {
            return FakeResponse::unprocessable(&format!(
                "PR {owner}/{repo}#{number} is not open (state={})",
                pr.state
            ));
        }
        if pr.mergeable == Some(false) || pr.mergeable_state == "dirty" {
            return FakeResponse::unprocessable(&format!(
                "PR {owner}/{repo}#{number} is not mergeable (state={})",
                pr.mergeable_state
            ));
        }
        pr.state = "merged".to_string();
        pr.merged_at = Some(format_now(now_ms));
        pr.mergeable_state = "clean".to_string();
        pr.head_sha.clone()
    };
    state.record(
        "fake_server:merge_pull",
        json!({"repo": repo, "number": number}),
    );
    FakeResponse::ok(json!({
        "sha": head_sha,
        "merged": true,
        "message": "Pull Request successfully merged"
    }))
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EnqueueMergeQueueBody {
    pub pull_number: u64,
    #[serde(default)]
    pub priority: Option<String>,
}

pub fn merge_queue_enqueue(
    state: &mut PlaygroundState,
    owner: &str,
    repo: &str,
    body: EnqueueMergeQueueBody,
) -> FakeResponse {
    let pr_key = PlaygroundPullRequest::compose_key(repo, body.pull_number);
    if state.owner != owner {
        return FakeResponse::not_found("unknown owner");
    }
    {
        let Some(pr) = state.pull_requests.get_mut(&pr_key) else {
            return FakeResponse::not_found(&format!(
                "PR {owner}/{repo}#{} not found",
                body.pull_number
            ));
        };
        if pr.state != "open" {
            return FakeResponse::unprocessable("PR is not open");
        }
        pr.merge_queue_status = Some("queued".to_string());
    }
    state.record(
        "fake_server:merge_queue_enqueue",
        json!({"repo": repo, "number": body.pull_number}),
    );
    FakeResponse::created(json!({
        "pull_number": body.pull_number,
        "status": "queued",
        "priority": body.priority
    }))
}

pub fn merge_queue_status(
    state: &PlaygroundState,
    owner: &str,
    repo: &str,
    base: &str,
) -> FakeResponse {
    if state.owner != owner || !state.repos.contains_key(repo) {
        return FakeResponse::not_found("unknown repo");
    }
    let mut entries: Vec<Value> = state
        .pull_requests
        .values()
        .filter(|pr| pr.repo == repo && pr.base_branch == base && pr.state == "open")
        .filter_map(|pr| {
            pr.merge_queue_status.as_ref().map(|status| {
                json!({
                    "pull_request_number": pr.number,
                    "status": status,
                    "head_branch": pr.head_branch
                })
            })
        })
        .collect();
    entries.sort_by_key(|v| v["pull_request_number"].as_u64().unwrap_or(0));
    FakeResponse::ok(json!({"base": base, "entries": entries}))
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SetLabelsBody {
    pub labels: Vec<String>,
}

pub fn set_labels(
    state: &mut PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
    body: SetLabelsBody,
) -> FakeResponse {
    let pr_key = PlaygroundPullRequest::compose_key(repo, number);
    if state.owner != owner {
        return FakeResponse::not_found("unknown owner");
    }
    let labels_now = {
        let Some(pr) = state.pull_requests.get_mut(&pr_key) else {
            return FakeResponse::not_found(&format!("PR {owner}/{repo}#{number} not found"));
        };
        pr.labels = body.labels;
        pr.labels.clone()
    };
    state.record(
        "fake_server:set_labels",
        json!({"repo": repo, "number": number}),
    );
    FakeResponse::ok(Value::Array(
        labels_now
            .iter()
            .map(|l| json!({"name": l, "color": "ededed"}))
            .collect(),
    ))
}

pub fn get_issue(state: &PlaygroundState, owner: &str, repo: &str, number: u64) -> FakeResponse {
    let Some(pr) = pr_lookup(state, owner, repo, number) else {
        return FakeResponse::not_found(&format!("issue {owner}/{repo}#{number} not found"));
    };
    FakeResponse::ok(json!({
        "number": pr.number,
        "title": pr.title,
        "body": pr.body,
        "state": pr.state,
        "user": {"login": pr.user},
        "labels": pr.labels.iter().map(|l| json!({"name": l, "color": "ededed"})).collect::<Vec<_>>(),
        "comments": pr.comments.len(),
        "html_url": format!("https://github.com/{}/{}/pull/{}", owner, repo, number)
    }))
}

fn pr_lookup<'a>(
    state: &'a PlaygroundState,
    owner: &str,
    repo: &str,
    number: u64,
) -> Option<&'a PlaygroundPullRequest> {
    if state.owner != owner {
        return None;
    }
    state
        .pull_requests
        .get(&PlaygroundPullRequest::compose_key(repo, number))
}

fn pr_to_v3(state: &PlaygroundState, pr: &PlaygroundPullRequest) -> Value {
    json!({
        "number": pr.number,
        "title": pr.title,
        "body": pr.body,
        "state": pr.state,
        "draft": pr.draft,
        "head": {
            "ref": pr.head_branch,
            "sha": pr.head_sha,
            "label": format!("{}:{}", state.owner, pr.head_branch)
        },
        "base": {
            "ref": pr.base_branch,
            "sha": Value::Null,
            "label": format!("{}:{}", state.owner, pr.base_branch)
        },
        "user": {"login": pr.user},
        "labels": pr.labels.iter().map(|l| json!({"name": l, "color": "ededed"})).collect::<Vec<_>>(),
        "mergeable": pr.mergeable,
        "mergeable_state": pr.mergeable_state,
        "merged": pr.state == "merged",
        "merged_at": pr.merged_at,
        "closed_at": pr.closed_at,
        "comments": pr.comments.len(),
        "auto_merge": pr.merge_queue_status.as_ref().map(|status| json!({"merge_method": "merge", "status": status})),
        "html_url": format!("https://github.com/{}/{}/pull/{}", state.owner, pr.repo, pr.number)
    })
}

fn format_now(now_ms: i64) -> String {
    use chrono::TimeZone;
    let utc = chrono::Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_else(chrono::Utc::now);
    utc.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Convenience helper used by the axum-based dispatcher to translate raw
/// query strings into a typed value.
pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            let v = u8::from_str_radix(hex, 16).unwrap_or(b' ');
            out.push(v as char);
            i += 3;
        } else {
            out.push(b as char);
            i += 1;
        }
    }
    out
}
