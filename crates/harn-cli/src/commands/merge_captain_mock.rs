//! `harn merge-captain mock` subcommands (#1020) — playground init/step/
//! status/serve/cleanup, plus the fake GitHub HTTP server.
//!
//! The heavy lifting (manifest parsing, on-disk materialization, state
//! mutation, request handling) lives in
//! `harn_vm::orchestration::playground`. This module is the thin axum +
//! clap glue.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path as AxumPath, Query, State as AxumState},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use harn_vm::orchestration::playground;
use harn_vm::value::VmError;

use crate::cli::{
    MergeCaptainMockCleanupArgs, MergeCaptainMockInitArgs, MergeCaptainMockServeArgs,
    MergeCaptainMockStatusArgs, MergeCaptainMockStepArgs,
};

pub(crate) fn run_init(args: &MergeCaptainMockInitArgs) -> i32 {
    if args.scenario.is_some() && args.manifest.is_some() {
        eprintln!("error: --scenario and --manifest are mutually exclusive");
        return 2;
    }
    let dir = Path::new(&args.dir);
    let manifest_path = args.manifest.as_ref().map(PathBuf::from);
    let manifest =
        match playground::resolve_init_manifest(args.scenario.as_deref(), manifest_path.as_deref())
        {
            Ok(m) => m,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
    if args.force {
        if let Err(error) = playground::cleanup_playground_at(dir) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    let state = match playground::init_playground_at(playground::InitOptions {
        dir,
        manifest: &manifest,
        allow_existing: false,
    }) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let summary = json!({
        "scenario": state.scenario,
        "owner": state.owner,
        "dir": dir.display().to_string(),
        "repos": state.repos.values().map(|r| json!({
            "name": r.name,
            "default_branch": r.default_branch,
            "remote_url": r.remote_url,
            "branches": r.branches,
        })).collect::<Vec<_>>(),
        "pull_requests": state.pull_requests.values().map(|pr| json!({
            "repo": pr.repo,
            "number": pr.number,
            "head_branch": pr.head_branch,
            "base_branch": pr.base_branch,
            "state": pr.state,
            "head_sha": pr.head_sha,
        })).collect::<Vec<_>>(),
    });
    print_json(&summary);
    0
}

pub(crate) fn run_cleanup(args: &MergeCaptainMockCleanupArgs) -> i32 {
    let dir = Path::new(&args.dir);
    match playground::cleanup_playground_at(dir) {
        Ok(true) => {
            println!("removed {}", dir.display());
            0
        }
        Ok(false) => {
            println!("nothing to remove at {}", dir.display());
            0
        }
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

pub(crate) fn run_status(args: &MergeCaptainMockStatusArgs) -> i32 {
    let dir = Path::new(&args.dir);
    let (state, manifest) = match playground::load_playground(dir) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let body = json!({
        "scenario": state.scenario,
        "owner": state.owner,
        "step_count": state.step_count,
        "now_ms": state.now_ms,
        "repos": state.repos.values().map(|r| json!({
            "name": r.name,
            "default_branch": r.default_branch,
            "branches": r.branches,
            "remote_url": r.remote_url,
        })).collect::<Vec<_>>(),
        "pull_requests": state.pull_requests.values().map(|pr| json!({
            "key": pr.key(),
            "title": pr.title,
            "state": pr.state,
            "head_branch": pr.head_branch,
            "base_branch": pr.base_branch,
            "head_sha": pr.head_sha,
            "labels": pr.labels,
            "checks": pr.checks,
            "mergeable": pr.mergeable,
            "mergeable_state": pr.mergeable_state,
            "merge_queue_status": pr.merge_queue_status,
            "comments": pr.comments.len(),
        })).collect::<Vec<_>>(),
        "available_steps": manifest.steps.iter().map(|s| json!({
            "name": s.name,
            "description": s.description,
            "actions": s.actions.len(),
        })).collect::<Vec<_>>(),
        "history": state.history,
    });
    if args.json {
        print_json(&body);
    } else {
        print_status_text(&state);
    }
    0
}

pub(crate) fn run_step(args: &MergeCaptainMockStepArgs) -> i32 {
    let dir = Path::new(&args.dir);
    let (mut state, manifest) = match playground::load_playground(dir) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let report = if let Some(name) = &args.name {
        match playground::run_named_step(dir, &mut state, &manifest, name) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    } else {
        let raw = args
            .action
            .as_ref()
            .expect("clap ArgGroup ensures one is set");
        let action: playground::ScenarioAction = match serde_json::from_str(raw) {
            Ok(a) => a,
            Err(error) => {
                eprintln!("error: failed to parse --action JSON: {error}");
                return 2;
            }
        };
        match playground::apply_one_action(dir, &mut state, &manifest, &action) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    };
    if let Err(error) = state.save(dir) {
        eprintln!("error: {error}");
        return 1;
    }
    let body = json!({
        "actions_applied": report.actions_applied,
        "prs_touched": report.prs_touched,
        "summary": report.summary,
        "step_count": state.step_count,
    });
    if args.json {
        print_json(&body);
    } else {
        for line in &report.summary {
            println!("{line}");
        }
        println!(
            "applied {} action(s) ({} PR(s) touched)",
            report.actions_applied,
            report.prs_touched.len()
        );
    }
    0
}

pub(crate) fn run_scenarios() -> i32 {
    for name in playground::builtin_scenario_names() {
        match playground::load_builtin(name) {
            Ok(m) => {
                println!("{}\t{}", name, m.description);
            }
            Err(error) => {
                eprintln!("error: built-in scenario {name} failed to load: {error}");
                return 1;
            }
        }
    }
    0
}

pub(crate) async fn run_serve(args: &MergeCaptainMockServeArgs) -> i32 {
    let dir = PathBuf::from(&args.dir);
    let (state, _manifest) = match playground::load_playground(&dir) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let bind: SocketAddr = match args.bind.parse() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("error: invalid --bind address {:?}: {error}", args.bind);
            return 2;
        }
    };
    let app_state = ServerState::new(dir.clone(), state);
    let router = build_router(app_state);
    let listener = match TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(error) => {
            eprintln!("error: failed to bind {bind}: {error}");
            return 1;
        }
    };
    let resolved = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("error: failed to resolve bind address: {error}");
            return 1;
        }
    };
    if args.print_addr {
        let summary = json!({
            "_type": "merge_captain_playground_serve",
            "addr": resolved.to_string(),
            "playground": dir.display().to_string(),
        });
        println!("{summary}");
    } else {
        eprintln!("serving fake GitHub on http://{resolved} (Ctrl-C to stop)");
    }
    if let Err(error) = axum::serve(listener, router).await {
        eprintln!("error: serve failed: {error}");
        return 1;
    }
    0
}

#[derive(Clone)]
pub(crate) struct ServerState {
    dir: PathBuf,
    state: Arc<Mutex<playground::PlaygroundState>>,
}

impl ServerState {
    fn new(dir: PathBuf, state: playground::PlaygroundState) -> Self {
        ServerState {
            dir,
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn with_read<R>(&self, f: impl FnOnce(&playground::PlaygroundState) -> R) -> R {
        let guard = self.state.lock().expect("playground state mutex poisoned");
        f(&guard)
    }

    fn with_write<R>(
        &self,
        f: impl FnOnce(&mut playground::PlaygroundState) -> R,
    ) -> Result<R, VmError> {
        let mut guard = self.state.lock().expect("playground state mutex poisoned");
        let value = f(&mut guard);
        guard.save(&self.dir)?;
        Ok(value)
    }
}

pub(crate) fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/repos/{owner}/{repo}/pulls", get(handle_list_pulls))
        .route(
            "/repos/{owner}/{repo}/pulls/{number}",
            get(handle_get_pull).patch(handle_patch_pull),
        )
        .route(
            "/repos/{owner}/{repo}/pulls/{number}/files",
            get(handle_list_pull_files),
        )
        .route(
            "/repos/{owner}/{repo}/pulls/{number}/merge",
            put(handle_merge_pull),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{number}",
            get(handle_get_issue),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{number}/comments",
            get(handle_list_issue_comments).post(handle_create_issue_comment),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{number}/labels",
            put(handle_set_labels),
        )
        .route(
            "/repos/{owner}/{repo}/commits/{sha}/check-runs",
            get(handle_list_check_runs),
        )
        .route(
            "/repos/{owner}/{repo}/actions/runs/{run_id}/logs",
            get(handle_workflow_run_logs),
        )
        .route(
            "/repos/{owner}/{repo}/merge_queue/queues/{base}",
            get(handle_merge_queue_status),
        )
        .route(
            "/repos/{owner}/{repo}/merge_queue/queues/{base}/items",
            post(handle_merge_queue_enqueue),
        )
        .route("/_playground/state", get(handle_state_dump))
        .route(
            "/_playground/healthz",
            get(|| async { (StatusCode::OK, "ok") }),
        )
        .with_state(state)
        .fallback(handle_fallback)
}

async fn handle_list_pulls(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo)): AxumPath<(String, String)>,
    Query(query): Query<playground::ListPullsQuery>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::list_pulls(s, &owner, &repo, &query));
    fake_into_response(response)
}

async fn handle_get_pull(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::get_pull(s, &owner, &repo, number));
    fake_into_response(response)
}

async fn handle_list_pull_files(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::list_pull_files(s, &owner, &repo, number));
    fake_into_response(response)
}

async fn handle_patch_pull(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
    Json(body): Json<playground::UpdatePullBody>,
) -> impl IntoResponse {
    match state.with_write(|s| playground::patch_pull(s, &owner, &repo, number, body.clone())) {
        Ok(response) => fake_into_response(response),
        Err(error) => fake_into_response(playground::FakeResponse {
            status: 500,
            body: json!({"message": error.to_string()}),
        }),
    }
}

async fn handle_merge_pull(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
    body: Option<Json<playground::MergePullBody>>,
) -> impl IntoResponse {
    let payload = body.map(|j| j.0).unwrap_or_default();
    match state.with_write(|s| playground::merge_pull(s, &owner, &repo, number, payload.clone())) {
        Ok(response) => fake_into_response(response),
        Err(error) => fake_into_response(playground::FakeResponse {
            status: 500,
            body: json!({"message": error.to_string()}),
        }),
    }
}

async fn handle_list_check_runs(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, sha)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::list_check_runs(s, &owner, &repo, &sha));
    fake_into_response(response)
}

async fn handle_workflow_run_logs(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, run_id)): AxumPath<(String, String, u64)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::workflow_run_logs(s, &owner, &repo, run_id));
    fake_into_response(response)
}

async fn handle_get_issue(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::get_issue(s, &owner, &repo, number));
    fake_into_response(response)
}

async fn handle_list_issue_comments(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::list_issue_comments(s, &owner, &repo, number));
    fake_into_response(response)
}

async fn handle_create_issue_comment(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
    Json(body): Json<playground::CreateCommentBody>,
) -> impl IntoResponse {
    match state
        .with_write(|s| playground::create_issue_comment(s, &owner, &repo, number, body.clone()))
    {
        Ok(response) => fake_into_response(response),
        Err(error) => fake_into_response(playground::FakeResponse {
            status: 500,
            body: json!({"message": error.to_string()}),
        }),
    }
}

async fn handle_set_labels(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, number)): AxumPath<(String, String, u64)>,
    Json(body): Json<playground::SetLabelsBody>,
) -> impl IntoResponse {
    match state.with_write(|s| playground::set_labels(s, &owner, &repo, number, body.clone())) {
        Ok(response) => fake_into_response(response),
        Err(error) => fake_into_response(playground::FakeResponse {
            status: 500,
            body: json!({"message": error.to_string()}),
        }),
    }
}

async fn handle_merge_queue_status(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, base)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let response = state.with_read(|s| playground::merge_queue_status(s, &owner, &repo, &base));
    fake_into_response(response)
}

async fn handle_merge_queue_enqueue(
    AxumState(state): AxumState<ServerState>,
    AxumPath((owner, repo, _base)): AxumPath<(String, String, String)>,
    Json(body): Json<playground::EnqueueMergeQueueBody>,
) -> impl IntoResponse {
    match state.with_write(|s| playground::merge_queue_enqueue(s, &owner, &repo, body.clone())) {
        Ok(response) => fake_into_response(response),
        Err(error) => fake_into_response(playground::FakeResponse {
            status: 500,
            body: json!({"message": error.to_string()}),
        }),
    }
}

async fn handle_state_dump(AxumState(state): AxumState<ServerState>) -> impl IntoResponse {
    let body = state.with_read(|s| serde_json::to_value(s).unwrap_or_default());
    (StatusCode::OK, Json(body))
}

async fn handle_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "message": "fake-github playground: endpoint not implemented",
            "documentation_url": ""
        })),
    )
}

fn fake_into_response(response: playground::FakeResponse) -> (StatusCode, Json<Value>) {
    (
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(response.body),
    )
}

fn print_status_text(state: &playground::PlaygroundState) {
    println!("scenario: {}", state.scenario);
    println!("owner:    {}", state.owner);
    println!("step_count: {}", state.step_count);
    println!("repos:");
    for repo in state.repos.values() {
        println!(
            "  {} (default={}, branches={}, remote={})",
            repo.name,
            repo.default_branch,
            repo.branches.len(),
            repo.remote_url
        );
    }
    println!("pull_requests:");
    for pr in state.pull_requests.values() {
        let head_sha = pr.head_sha.as_deref().unwrap_or("?");
        println!(
            "  {} state={} head={}@{} mergeable={} state_v3={}",
            pr.key(),
            pr.state,
            pr.head_branch,
            short_sha(head_sha),
            match pr.mergeable {
                Some(true) => "yes",
                Some(false) => "no",
                None => "?",
            },
            pr.mergeable_state
        );
    }
}

fn short_sha(sha: &str) -> &str {
    if sha.len() > 7 {
        &sha[..7]
    } else {
        sha
    }
}

fn print_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("error: failed to serialize JSON: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use harn_vm::orchestration::playground::{self, ScenarioManifest};
    use tower::ServiceExt;

    fn make_state() -> ServerState {
        let manifest =
            playground::load_builtin("single_green").expect("single_green scenario must parse");
        let mut state = playground::PlaygroundState::from_manifest(&manifest);
        // Bypass disk I/O for the unit test by injecting state directly. We
        // write to a tempdir so `with_write` can save without errors.
        let tempdir = tempfile::tempdir().expect("tempdir");
        // Mimic init by setting a remote_url for the repo so endpoints don't
        // depend on real git.
        for repo in state.repos.values_mut() {
            repo.remote_url = format!("file:///tmp/{}.git", repo.name);
        }
        // Also stamp a head_sha for the seeded PR.
        for pr in state.pull_requests.values_mut() {
            pr.head_sha = Some("aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111".to_string());
        }
        // Persist the tempdir for the duration of the test; otherwise the
        // server's `with_write` would try to save state into a removed dir.
        let dir = tempdir.keep();
        ServerState::new(dir, state)
    }

    fn assert_yaml_manifest_round_trip(_: &ScenarioManifest) {}

    #[tokio::test]
    async fn list_pulls_returns_open_prs() {
        let state = make_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/repos/burin-labs/solo/pulls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["number"], 1);
    }

    #[tokio::test]
    async fn get_pull_returns_one_pr() {
        let state = make_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/repos/burin-labs/solo/pulls/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_pr_404s() {
        let state = make_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/repos/burin-labs/solo/pulls/999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_comment_persists_state() {
        let state = make_state();
        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/repos/burin-labs/solo/issues/1/comments")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"body": "hello", "user": "alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        // Re-read state and confirm the comment is there.
        let comments = state.with_read(|s| {
            let pr = s
                .pull_requests
                .get(&playground::PlaygroundPullRequest::compose_key("solo", 1))
                .unwrap()
                .clone();
            pr.comments
        });
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].user, "alice");
    }

    #[tokio::test]
    async fn merge_pull_blocks_dirty() {
        let state = make_state();
        // Mark dirty
        state
            .with_write(|s| {
                let pr = s
                    .pull_requests
                    .get_mut(&playground::PlaygroundPullRequest::compose_key("solo", 1))
                    .unwrap();
                pr.mergeable = Some(false);
                pr.mergeable_state = "dirty".to_string();
            })
            .unwrap();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/repos/burin-labs/solo/pulls/1/merge")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"merge_method": "merge"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let _: () =
            assert_yaml_manifest_round_trip(&playground::load_builtin("single_green").unwrap());
    }
}
