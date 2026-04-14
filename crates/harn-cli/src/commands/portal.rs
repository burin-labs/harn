use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Json as ExtractJson, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Mutex;

use harn_lexer::KEYWORDS;
use harn_vm::llm_config;
use harn_vm::stdlib::stdlib_builtin_names;

static PORTAL_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/portal-dist");

#[derive(Clone)]
struct PortalState {
    run_dir: PathBuf,
    workspace_root: PathBuf,
    launch_program: PathBuf,
    launch_jobs: Arc<Mutex<HashMap<String, PortalLaunchJob>>>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalStats {
    total_runs: usize,
    completed_runs: usize,
    active_runs: usize,
    failed_runs: usize,
    avg_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct PortalRunSummary {
    path: String,
    id: String,
    workflow_name: String,
    status: String,
    last_stage_node_id: Option<String>,
    failure_summary: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u64>,
    stage_count: usize,
    child_run_count: usize,
    call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    models: Vec<String>,
    updated_at_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
struct PortalInsight {
    label: String,
    value: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalStage {
    id: String,
    node_id: String,
    kind: String,
    status: String,
    outcome: String,
    branch: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u64>,
    artifact_count: usize,
    attempt_count: usize,
    verification_summary: Option<String>,
    debug: PortalStageDebug,
}

#[derive(Debug, Clone, Serialize)]
struct PortalStageDebug {
    call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    consumed_artifact_ids: Vec<String>,
    produced_artifact_ids: Vec<String>,
    selected_artifact_ids: Vec<String>,
    worker_id: Option<String>,
    error: Option<String>,
    model_policy: Option<String>,
    transcript_policy: Option<String>,
    context_policy: Option<String>,
    retry_policy: Option<String>,
    capability_policy: Option<String>,
    input_contract: Option<String>,
    output_contract: Option<String>,
    prompt: Option<String>,
    system_prompt: Option<String>,
    rendered_context: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalSpan {
    span_id: u64,
    parent_id: Option<u64>,
    kind: String,
    name: String,
    start_ms: u64,
    duration_ms: u64,
    end_ms: u64,
    label: String,
    lane: usize,
    depth: usize,
    metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalActivity {
    label: String,
    kind: String,
    started_offset_ms: u64,
    duration_ms: u64,
    stage_node_id: Option<String>,
    call_id: Option<String>,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalArtifact {
    id: String,
    kind: String,
    title: String,
    source: Option<String>,
    stage: Option<String>,
    estimated_tokens: Option<usize>,
    lineage_count: usize,
    preview: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalTransition {
    from_node_id: Option<String>,
    to_node_id: String,
    branch: Option<String>,
    consumed_count: usize,
    produced_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PortalCheckpoint {
    reason: String,
    ready_count: usize,
    completed_count: usize,
    last_stage_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalExecutionSummary {
    cwd: Option<String>,
    repo_path: Option<String>,
    worktree_path: Option<String>,
    branch: Option<String>,
    adapter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalPolicySummary {
    tools: Vec<String>,
    capabilities: Vec<String>,
    workspace_roots: Vec<String>,
    side_effect_level: Option<String>,
    recursion_limit: Option<usize>,
    tool_arg_constraints: Vec<String>,
    validation_valid: Option<bool>,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
    reachable_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalReplayAssertion {
    node_id: String,
    expected_status: String,
    expected_outcome: String,
    expected_branch: Option<String>,
    required_artifact_kinds: Vec<String>,
    visible_text_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalReplaySummary {
    fixture_id: String,
    source_run_id: String,
    created_at: String,
    expected_status: String,
    stage_assertions: Vec<PortalReplayAssertion>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalTranscriptMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalTranscriptStep {
    call_id: String,
    span_id: Option<u64>,
    iteration: usize,
    call_index: usize,
    model: String,
    provider: Option<String>,
    kept_messages: usize,
    added_messages: usize,
    total_messages: usize,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    system_prompt: Option<String>,
    added_context: Vec<PortalTranscriptMessage>,
    response_text: Option<String>,
    thinking: Option<String>,
    tool_calls: Vec<String>,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalStorySection {
    title: String,
    scope: String,
    role: String,
    source: String,
    text: String,
    preview: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalChildRun {
    worker_name: String,
    status: String,
    started_at: String,
    finished_at: Option<String>,
    run_id: Option<String>,
    run_path: Option<String>,
    task: String,
}

#[derive(Debug, Clone, Serialize)]
struct PortalRunDetail {
    summary: PortalRunSummary,
    task: String,
    workflow_id: String,
    parent_run_id: Option<String>,
    root_run_id: Option<String>,
    policy_summary: PortalPolicySummary,
    replay_summary: Option<PortalReplaySummary>,
    execution: Option<harn_vm::orchestration::RunExecutionRecord>,
    insights: Vec<PortalInsight>,
    stages: Vec<PortalStage>,
    spans: Vec<PortalSpan>,
    activities: Vec<PortalActivity>,
    transitions: Vec<PortalTransition>,
    checkpoints: Vec<PortalCheckpoint>,
    artifacts: Vec<PortalArtifact>,
    execution_summary: Option<PortalExecutionSummary>,
    transcript_steps: Vec<PortalTranscriptStep>,
    story: Vec<PortalStorySection>,
    child_runs: Vec<PortalChildRun>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalLaunchTarget {
    path: String,
    group: String,
}

#[derive(Debug, Clone)]
struct MaterializedLaunchTarget {
    mode: String,
    target_label: String,
    launch_file: PathBuf,
    workspace_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct PortalLaunchJob {
    id: String,
    mode: String,
    target_label: String,
    status: String,
    started_at: String,
    finished_at: Option<String>,
    exit_code: Option<i32>,
    logs: String,
    discovered_run_paths: Vec<String>,
    workspace_dir: Option<String>,
    transcript_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct PortalLaunchTargetList {
    targets: Vec<PortalLaunchTarget>,
}

#[derive(Debug, Serialize)]
struct PortalLaunchJobList {
    jobs: Vec<PortalLaunchJob>,
}

#[derive(Debug, Deserialize)]
struct PortalLaunchRequest {
    file_path: Option<String>,
    source: Option<String>,
    task: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct PortalRunDiff {
    left_path: String,
    right_path: String,
    identical: bool,
    status_changed: bool,
    left_status: String,
    right_status: String,
    stage_diffs: Vec<harn_vm::orchestration::RunStageDiffRecord>,
    transition_count_delta: isize,
    artifact_count_delta: isize,
    checkpoint_count_delta: isize,
}

#[derive(Debug, Serialize)]
struct PortalListResponse {
    stats: PortalStats,
    filtered_count: usize,
    pagination: PortalPagination,
    runs: Vec<PortalRunSummary>,
}

#[derive(Debug, Serialize)]
struct PortalPagination {
    page: usize,
    page_size: usize,
    total_pages: usize,
    total_runs: usize,
    has_previous: bool,
    has_next: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct PortalMeta {
    workspace_root: String,
    run_dir: String,
}

#[derive(Debug, Serialize)]
struct PortalHighlightKeywords {
    keyword: Vec<String>,
    literal: Vec<String>,
    built_in: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PortalLlmProviderOption {
    name: String,
    base_url: String,
    base_url_env: Option<String>,
    auth_style: String,
    auth_envs: Vec<String>,
    auth_configured: bool,
    viable: bool,
    local: bool,
    models: Vec<String>,
    aliases: Vec<String>,
    default_model: String,
}

#[derive(Debug, Serialize)]
struct PortalLlmOptions {
    preferred_provider: Option<String>,
    preferred_model: Option<String>,
    providers: Vec<PortalLlmProviderOption>,
}

#[derive(Debug, serde::Deserialize)]
struct RunQuery {
    path: String,
}

#[derive(Debug, serde::Deserialize)]
struct CompareQuery {
    left: String,
    right: String,
}

#[derive(Debug, serde::Deserialize)]
struct ListRunsQuery {
    q: Option<String>,
    workflow: Option<String>,
    status: Option<String>,
    sort: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
}

pub(crate) async fn run_portal(dir: &str, host: &str, port: u16, open_browser: bool) {
    let run_dir = PathBuf::from(dir);
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let launch_program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("harn"));
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: invalid portal bind address {host}:{port}: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(PortalState {
        run_dir: run_dir.clone(),
        workspace_root,
        launch_program,
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
    });
    let app = build_router(state);
    let url = format!("http://{addr}");

    println!("Harn portal listening on {url}");
    println!("Watching run records in {}", run_dir.display());

    if open_browser {
        let _ = webbrowser::open(&url);
    }

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind portal listener on {addr}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("error: portal server failed: {e}");
        std::process::exit(1);
    }
}

fn build_router(state: Arc<PortalState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/meta", get(portal_meta_handler))
        .route("/api/highlight/keywords", get(highlight_keywords_handler))
        .route("/api/llm/options", get(llm_options_handler))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/run", get(run_detail_handler))
        .route("/api/compare", get(compare_runs_handler))
        .route("/api/launch/targets", get(list_launch_targets_handler))
        .route("/api/launch/jobs", get(list_launch_jobs_handler))
        .route("/api/launch", post(launch_run_handler))
        .route("/{*path}", get(index))
        .with_state(state)
}

async fn index() -> Response {
    match PORTAL_DIST.get_file("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.contents()).into_owned()).into_response(),
        None => internal_error("portal frontend is not built; run npm install && npm run build in crates/harn-cli/portal")
            .into_response(),
    }
}

async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let asset_path = format!("assets/{path}");
    match PORTAL_DIST.get_file(&asset_path) {
        Some(file) => asset_response(file.contents(), content_type_for_path(&asset_path)),
        None => not_found_error(format!("asset not found: {path}")).into_response(),
    }
}

async fn list_runs_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<ListRunsQuery>,
) -> Result<Json<PortalListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let runs = scan_runs(&state.run_dir).map_err(internal_error)?;
    let stats = summarize_runs(&runs);
    let page_size = query.page_size.unwrap_or(25).clamp(1, 200);
    let page = query.page.unwrap_or(1).max(1);
    let filtered = filter_and_sort_runs(runs, &query);
    let filtered_count = filtered.len();
    let total_pages = usize::max(1, filtered_count.div_ceil(page_size));
    let clamped_page = page.min(total_pages);
    let start = (clamped_page - 1) * page_size;
    let end = usize::min(start + page_size, filtered_count);
    let page_runs = filtered
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect::<Vec<_>>();
    Ok(Json(PortalListResponse {
        stats,
        filtered_count,
        pagination: PortalPagination {
            page: clamped_page,
            page_size,
            total_pages,
            total_runs: filtered_count,
            has_previous: clamped_page > 1,
            has_next: clamped_page < total_pages,
        },
        runs: page_runs,
    }))
}

async fn portal_meta_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalMeta>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(PortalMeta {
        workspace_root: state.workspace_root.display().to_string(),
        run_dir: state.run_dir.display().to_string(),
    }))
}

async fn highlight_keywords_handler(
) -> Result<Json<PortalHighlightKeywords>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(build_highlight_keywords()))
}

async fn llm_options_handler() -> Result<Json<PortalLlmOptions>, (StatusCode, Json<ErrorResponse>)>
{
    let options = build_llm_options().await;
    Ok(Json(options))
}

async fn run_detail_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<RunQuery>,
) -> Result<Json<PortalRunDetail>, (StatusCode, Json<ErrorResponse>)> {
    let path = resolve_run_path(&state.run_dir, &query.path)?;
    let run = harn_vm::orchestration::load_run_record(&path).map_err(|error| {
        if path.exists() {
            internal_error(format!("failed to load run record: {error}"))
        } else {
            not_found_error(format!("run record not found: {}", query.path))
        }
    })?;
    Ok(Json(build_run_detail(&state.run_dir, &query.path, &run)))
}

async fn compare_runs_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<CompareQuery>,
) -> Result<Json<PortalRunDiff>, (StatusCode, Json<ErrorResponse>)> {
    let left_path = resolve_run_path(&state.run_dir, &query.left)?;
    let right_path = resolve_run_path(&state.run_dir, &query.right)?;
    let left = harn_vm::orchestration::load_run_record(&left_path).map_err(|error| {
        if left_path.exists() {
            internal_error(format!("failed to load left run: {error}"))
        } else {
            not_found_error(format!("left run not found: {}", query.left))
        }
    })?;
    let right = harn_vm::orchestration::load_run_record(&right_path).map_err(|error| {
        if right_path.exists() {
            internal_error(format!("failed to load right run: {error}"))
        } else {
            not_found_error(format!("right run not found: {}", query.right))
        }
    })?;
    let diff = harn_vm::orchestration::diff_run_records(&left, &right);
    Ok(Json(PortalRunDiff {
        left_path: query.left,
        right_path: query.right,
        identical: diff.identical,
        status_changed: diff.status_changed,
        left_status: diff.left_status,
        right_status: diff.right_status,
        stage_diffs: diff.stage_diffs,
        transition_count_delta: diff.transition_count_delta,
        artifact_count_delta: diff.artifact_count_delta,
        checkpoint_count_delta: diff.checkpoint_count_delta,
    }))
}

async fn list_launch_targets_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalLaunchTargetList>, (StatusCode, Json<ErrorResponse>)> {
    let targets = scan_launch_targets(&state.workspace_root).map_err(internal_error)?;
    Ok(Json(PortalLaunchTargetList { targets }))
}

async fn list_launch_jobs_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalLaunchJobList>, (StatusCode, Json<ErrorResponse>)> {
    let jobs = state
        .launch_jobs
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    Ok(Json(PortalLaunchJobList { jobs }))
}

async fn launch_run_handler(
    State(state): State<Arc<PortalState>>,
    ExtractJson(request): ExtractJson<PortalLaunchRequest>,
) -> Result<Json<PortalLaunchJob>, (StatusCode, Json<ErrorResponse>)> {
    let job = create_launch_job(&state, request).await?;
    Ok(Json(job))
}

async fn create_launch_job(
    state: &Arc<PortalState>,
    request: PortalLaunchRequest,
) -> Result<PortalLaunchJob, (StatusCode, Json<ErrorResponse>)> {
    validate_launch_request(&request)?;

    let job_id = portal_timestamp_id("job");
    let started_at = portal_timestamp_id("started");
    let before_paths = known_run_paths(&state.run_dir).map_err(internal_error)?;
    let launch_env = validated_env_overrides(request.env.as_ref())?;
    let materialized =
        materialize_launch_target(&state.run_dir, &state.workspace_root, &job_id, request)
            .map_err(internal_error)?;
    let transcript_path = materialized.workspace_dir.as_ref().map(|dir| {
        dir.join("run-llm")
            .join("llm_transcript.jsonl")
            .display()
            .to_string()
    });

    let job = PortalLaunchJob {
        id: job_id.clone(),
        mode: materialized.mode.clone(),
        target_label: materialized.target_label.clone(),
        status: "running".to_string(),
        started_at,
        finished_at: None,
        exit_code: None,
        logs: String::new(),
        discovered_run_paths: Vec::new(),
        workspace_dir: materialized
            .workspace_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        transcript_path,
    };
    state
        .launch_jobs
        .lock()
        .await
        .insert(job_id.clone(), job.clone());

    let jobs = state.launch_jobs.clone();
    let launch_program = state.launch_program.clone();
    let run_dir = state.run_dir.clone();
    let workspace_root = state.workspace_root.clone();
    tokio::spawn(async move {
        let output = Command::new(&launch_program)
            .arg("run")
            .arg(&materialized.launch_file)
            .current_dir(&workspace_root)
            .envs(build_launch_env(
                materialized.workspace_dir.as_deref(),
                &launch_env,
            ))
            .output()
            .await;

        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            match output {
                Ok(output) => {
                    let mut logs = String::new();
                    logs.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !output.stderr.is_empty() {
                        if !logs.is_empty() {
                            logs.push('\n');
                        }
                        logs.push_str(&String::from_utf8_lossy(&output.stderr));
                    }
                    job.logs = logs;
                    job.exit_code = output.status.code();
                    job.status = if output.status.success() {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    };
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.discovered_run_paths =
                        discovered_run_paths(&run_dir, &before_paths).unwrap_or_default();
                }
                Err(error) => {
                    job.status = "failed".to_string();
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.logs = format!("failed to start harn run: {error}");
                }
            }
        }
    });

    Ok(job)
}

fn asset_response(body: &'static [u8], content_type: &'static str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        content_type.parse().expect("content type"),
    );
    (headers, body).into_response()
}

fn content_type_for_path(path: &str) -> &'static str {
    if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

fn internal_error(message: impl ToString) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

fn build_highlight_keywords() -> PortalHighlightKeywords {
    let literals = ["true", "false", "nil"];
    let literal_set = literals.into_iter().collect::<HashSet<_>>();
    let keyword = KEYWORDS
        .iter()
        .filter(|item| !literal_set.contains(**item))
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    let keyword_set = KEYWORDS.iter().copied().collect::<HashSet<_>>();
    let mut built_in = stdlib_builtin_names()
        .into_iter()
        .filter(|name| !name.starts_with("__"))
        .filter(|name| !keyword_set.contains(name.as_str()))
        .collect::<Vec<_>>();
    built_in.sort();
    built_in.dedup();
    PortalHighlightKeywords {
        keyword,
        literal: literals.into_iter().map(str::to_string).collect(),
        built_in,
    }
}

async fn build_llm_options() -> PortalLlmOptions {
    let config = llm_config::load_config();
    let preferred_provider = std::env::var("HARN_LLM_PROVIDER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok() {
                Some("local".to_string())
            } else {
                None
            }
        });
    let preferred_model = std::env::var("HARN_LLM_MODEL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LOCAL_LLM_MODEL")
                .ok()
                .filter(|value| !value.is_empty())
        });

    let mut providers = Vec::new();
    for name in llm_config::provider_names() {
        let Some(def) = llm_config::provider_config(&name) else {
            continue;
        };
        let base_url = llm_config::resolve_base_url(def);
        let auth_envs = auth_env_names(&def.auth_env);
        let auth_configured = auth_envs.iter().any(|env_name| {
            std::env::var(env_name)
                .ok()
                .is_some_and(|value| !value.is_empty())
        });
        let viable = def.auth_style == "none" || auth_configured;
        let local = is_local_provider(&base_url);
        let aliases = config
            .aliases
            .iter()
            .filter(|(_, alias)| alias.provider == name)
            .map(|(alias_name, _)| alias_name.clone())
            .collect::<Vec<_>>();
        let mut models = if local {
            discover_provider_models(&name, &base_url, def)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Some(default_model) = default_model_for_provider(&name) {
            if !models.contains(&default_model) {
                models.insert(0, default_model.clone());
            }
        }
        for alias_name in &aliases {
            if let Some((resolved, _)) = llm_config::resolve_tier_model(alias_name, Some(&name)) {
                if !models.contains(&resolved) {
                    models.push(resolved);
                }
            }
        }
        models.sort();
        models.dedup();
        providers.push(PortalLlmProviderOption {
            name: name.clone(),
            base_url,
            base_url_env: def.base_url_env.clone(),
            auth_style: def.auth_style.clone(),
            auth_envs,
            auth_configured,
            viable,
            local,
            models,
            aliases,
            default_model: default_model_for_provider(&name).unwrap_or_default(),
        });
    }

    providers.sort_by(|left, right| {
        right
            .viable
            .cmp(&left.viable)
            .then_with(|| right.local.cmp(&left.local))
            .then_with(|| left.name.cmp(&right.name))
    });

    PortalLlmOptions {
        preferred_provider,
        preferred_model,
        providers,
    }
}

fn auth_env_names(auth_env: &llm_config::AuthEnv) -> Vec<String> {
    match auth_env {
        llm_config::AuthEnv::None => Vec::new(),
        llm_config::AuthEnv::Single(name) => vec![name.clone()],
        llm_config::AuthEnv::Multiple(names) => names.clone(),
    }
}

fn is_local_provider(base_url: &str) -> bool {
    base_url.contains("127.0.0.1") || base_url.contains("localhost")
}

fn default_model_for_provider(provider: &str) -> Option<String> {
    match provider {
        "local" => std::env::var("LOCAL_LLM_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("HARN_LLM_MODEL")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| Some("gpt-4o".to_string())),
        "openai" => Some("gpt-4o".to_string()),
        "ollama" => Some("llama3.2".to_string()),
        "openrouter" => Some("Qwen/Qwen3.5-9B".to_string()),
        "anthropic" => Some("claude-sonnet-4-20250514".to_string()),
        _ => None,
    }
}

async fn discover_provider_models(
    provider: &str,
    base_url: &str,
    def: &llm_config::ProviderDef,
) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|error| format!("failed to build model discovery client: {error}"))?;

    let response = if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        client
            .get(format!("{base_url}/api/tags"))
            .send()
            .await
            .map_err(|error| format!("failed to reach {provider}: {error}"))?
    } else {
        client
            .get(format!("{base_url}/v1/models"))
            .send()
            .await
            .map_err(|error| format!("failed to reach {provider}: {error}"))?
    };
    if !response.status().is_success() {
        return Ok(Vec::new());
    }
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("failed to parse model list: {error}"))?;
    let mut models = Vec::new();
    if provider == "ollama" || def.chat_endpoint.contains("/api/chat") {
        if let Some(entries) = payload.get("models").and_then(|value| value.as_array()) {
            for entry in entries {
                if let Some(name) = entry.get("name").and_then(|value| value.as_str()) {
                    models.push(name.to_string());
                }
            }
        }
    } else if let Some(entries) = payload.get("data").and_then(|value| value.as_array()) {
        for entry in entries {
            if let Some(id) = entry.get("id").and_then(|value| value.as_str()) {
                models.push(id.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    Ok(models)
}

fn bad_request_error(message: impl ToString) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

fn not_found_error(message: impl ToString) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

fn validate_launch_request(
    request: &PortalLaunchRequest,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let has_file = request
        .file_path
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_source = request
        .source
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_playground = request
        .task
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let count = [has_file, has_source, has_playground]
        .into_iter()
        .filter(|value| *value)
        .count();
    if count != 1 {
        return Err(bad_request_error(
            "launch requires exactly one of file_path, source, or task",
        ));
    }
    Ok(())
}

fn validated_env_overrides(
    env: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, (StatusCode, Json<ErrorResponse>)> {
    let mut validated = BTreeMap::new();
    if let Some(env) = env {
        for (key, value) in env {
            if key.is_empty()
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            {
                return Err(bad_request_error(format!(
                    "invalid env key '{key}'; use uppercase shell-style names only"
                )));
            }
            if value.len() > 16_384 {
                return Err(bad_request_error(format!(
                    "env value for '{key}' is too large"
                )));
            }
            validated.insert(key.clone(), value.clone());
        }
    }
    Ok(validated)
}

fn build_launch_env(
    workspace_dir: Option<&Path>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = overrides.clone();
    if let Some(workspace_dir) = workspace_dir {
        env.insert(
            "HARN_LLM_TRANSCRIPT_DIR".to_string(),
            workspace_dir.join("run-llm").display().to_string(),
        );
    }
    env
}

fn materialize_launch_target(
    run_dir: &Path,
    workspace_root: &Path,
    job_id: &str,
    request: PortalLaunchRequest,
) -> Result<MaterializedLaunchTarget, String> {
    if let Some(file_path) = request.file_path.filter(|value| !value.trim().is_empty()) {
        let path = resolve_workspace_file(workspace_root, &file_path)?;
        return Ok(MaterializedLaunchTarget {
            mode: "file".to_string(),
            target_label: file_path,
            launch_file: path,
            workspace_dir: None,
        });
    }

    if let Some(source) = request.source.filter(|value| !value.trim().is_empty()) {
        let workspace_dir = launch_workspace_dir(run_dir, job_id)?;
        let launch_file = workspace_dir.join("workflow.harn");
        fs::write(&launch_file, source)
            .map_err(|error| format!("failed to write temp source file: {error}"))?;
        write_launch_metadata(
            &workspace_dir,
            &serde_json::json!({
                "mode": "source",
                "source_path": launch_file.display().to_string(),
            }),
        )?;
        return Ok(MaterializedLaunchTarget {
            mode: "source".to_string(),
            target_label: launch_file.display().to_string(),
            launch_file,
            workspace_dir: Some(workspace_dir),
        });
    }

    let task = request
        .task
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "playground task is required".to_string())?;
    let workspace_dir = launch_workspace_dir(run_dir, job_id)?;
    let source = build_playground_source(
        &workspace_dir,
        &task,
        request.provider.as_deref(),
        request.model.as_deref(),
    );
    let launch_file = workspace_dir.join("workflow.harn");
    fs::write(&launch_file, source)
        .map_err(|error| format!("failed to write playground source file: {error}"))?;
    fs::write(workspace_dir.join("task.txt"), &task)
        .map_err(|error| format!("failed to write playground task file: {error}"))?;
    write_launch_metadata(
        &workspace_dir,
        &serde_json::json!({
            "mode": "playground",
            "task": task,
            "provider": request.provider.as_deref(),
            "model": request.model.as_deref(),
            "workflow_path": launch_file.display().to_string(),
            "run_path": workspace_dir.join("run.json").display().to_string(),
            "transcript_path": workspace_dir.join("run-llm").join("llm_transcript.jsonl").display().to_string(),
        }),
    )?;
    Ok(MaterializedLaunchTarget {
        mode: "playground".to_string(),
        target_label: format!("playground: {task}"),
        launch_file,
        workspace_dir: Some(workspace_dir),
    })
}

fn launch_workspace_dir(run_dir: &Path, job_id: &str) -> Result<PathBuf, String> {
    let dir = run_dir.join("playground").join(job_id);
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create launch workspace {}: {error}",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn write_launch_metadata(workspace_dir: &Path, payload: &serde_json::Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(payload)
        .map_err(|error| format!("failed to encode launch metadata: {error}"))?;
    fs::write(workspace_dir.join("launch.json"), content)
        .map_err(|error| format!("failed to write launch metadata: {error}"))
}

fn resolve_workspace_file(workspace_root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if path.is_absolute() {
        return Err("file_path must be relative to the current workspace".to_string());
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err("file_path must stay inside the current workspace".to_string());
    }
    let resolved = workspace_root.join(path);
    if !resolved.exists() {
        return Err(format!("file not found: {relative_path}"));
    }
    Ok(resolved)
}

fn build_playground_source(
    workspace_dir: &Path,
    task: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> String {
    let persist_path = workspace_dir.join("run.json");
    let provider_line = provider
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("provider: {:?},", value))
        .unwrap_or_default();
    let model_line = model
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("model: {:?},", value))
        .unwrap_or_default();

    format!(
        r#"pipeline main() {{
  let flow = workflow_graph(
    {{
    name: "portal_playground",
    entry: "act",
    nodes: {{
    act: {{
    kind: "stage",
    mode: "llm",
    model_policy: {{{provider_line}{model_line}}},
    output_contract: {{output_kinds: ["summary"]}},
  }},
  }},
    edges: [],
  }},
  )
  let seed = artifact({{kind: "summary", text: "Playground seed context", relevance: 0.5}})
  let workspace_note = artifact({{
    kind: "workspace_file",
    title: "task.txt",
    text: {task:?},
    relevance: 0.9,
  }})
  let result = workflow_execute(
    {task:?},
    flow,
    [seed, workspace_note],
    {{max_steps: 4, persist_path: {:?}}},
  )
  println(result?.status)
  println(result?.run?.persisted_path)
}}"#,
        persist_path.display().to_string()
    )
}

fn known_run_paths(run_dir: &Path) -> Result<HashSet<String>, String> {
    Ok(scan_runs(run_dir)?
        .into_iter()
        .map(|run| run.path)
        .collect::<HashSet<_>>())
}

fn discovered_run_paths(
    run_dir: &Path,
    before_paths: &HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut paths = scan_runs(run_dir)?
        .into_iter()
        .map(|run| run.path)
        .filter(|path| !before_paths.contains(path))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn scan_launch_targets(workspace_root: &Path) -> Result<Vec<PortalLaunchTarget>, String> {
    let groups = [
        ("examples", workspace_root.join("examples")),
        ("conformance", workspace_root.join("conformance/tests")),
    ];
    let mut targets = Vec::new();
    for (group, root) in groups {
        collect_launch_targets(workspace_root, &root, group, &mut targets)?;
    }
    targets.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(targets)
}

fn collect_launch_targets(
    workspace_root: &Path,
    current: &Path,
    group: &str,
    out: &mut Vec<PortalLaunchTarget>,
) -> Result<(), String> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)
        .map_err(|error| format!("failed to read {}: {error}", current.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to iterate {}: {error}", current.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_launch_targets(workspace_root, &path, group, out)?;
        } else if path.extension().is_some_and(|ext| ext == "harn") {
            let relative = path
                .strip_prefix(workspace_root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?
                .display()
                .to_string();
            out.push(PortalLaunchTarget {
                path: relative,
                group: group.to_string(),
            });
        }
    }
    Ok(())
}

fn scan_runs(run_dir: &Path) -> Result<Vec<PortalRunSummary>, String> {
    let mut files = Vec::new();
    collect_run_files(run_dir, run_dir, &mut files)?;

    let mut runs = Vec::new();
    for (path, modified_at_ms) in files {
        if let Ok(run) = harn_vm::orchestration::load_run_record(&path) {
            if run.type_name != "run_record" || run.id.is_empty() || run.workflow_id.is_empty() {
                continue;
            }
            let relative = path
                .strip_prefix(run_dir)
                .ok()
                .unwrap_or(&path)
                .display()
                .to_string();
            runs.push(build_run_summary(&relative, modified_at_ms, &run));
        }
    }

    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
    });
    Ok(runs)
}

fn filter_and_sort_runs(
    runs: Vec<PortalRunSummary>,
    query: &ListRunsQuery,
) -> Vec<PortalRunSummary> {
    let search = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let workflow = query
        .workflow
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let status = query.status.as_deref().unwrap_or("all");
    let sort = query.sort.as_deref().unwrap_or("newest");

    let mut filtered = runs
        .into_iter()
        .filter(|run| match workflow.as_deref() {
            Some(name) => run.workflow_name == name,
            None => true,
        })
        .filter(|run| matches_run_status(run, status))
        .filter(|run| match search.as_deref() {
            Some(needle) => matches_run_search(run, needle),
            None => true,
        })
        .collect::<Vec<_>>();

    filtered.sort_by(|left, right| match sort {
        "duration" => right
            .duration_ms
            .unwrap_or_default()
            .cmp(&left.duration_ms.unwrap_or_default())
            .then_with(|| right.started_at.cmp(&left.started_at)),
        "oldest" => left
            .started_at
            .cmp(&right.started_at)
            .then_with(|| left.updated_at_ms.cmp(&right.updated_at_ms)),
        _ => right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms)),
    });
    filtered
}

fn matches_run_status(run: &PortalRunSummary, status: &str) -> bool {
    match status {
        "failed" => is_failed_status(&run.status),
        "completed" => is_completed_status(&run.status),
        "active" => !is_failed_status(&run.status) && !is_completed_status(&run.status),
        _ => true,
    }
}

fn matches_run_search(run: &PortalRunSummary, needle: &str) -> bool {
    let models = run.models.join(" ");
    format!(
        "{} {} {} {} {} {}",
        run.workflow_name,
        run.status,
        run.path,
        run.last_stage_node_id.as_deref().unwrap_or_default(),
        run.failure_summary.as_deref().unwrap_or_default(),
        models
    )
    .to_ascii_lowercase()
    .contains(needle)
}

fn collect_run_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(PathBuf, u128)>,
) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|error| format!("failed to read {}: {error}", current.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to iterate {}: {error}", current.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_run_files(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let modified_at_ms = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(system_time_ms)
            .unwrap_or_else(|| system_time_ms(SystemTime::now()).unwrap_or(0));
        if path.starts_with(root) {
            out.push((path, modified_at_ms));
        }
    }
    Ok(())
}

fn resolve_run_path(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, (StatusCode, Json<ErrorResponse>)> {
    if relative.trim().is_empty() {
        return Err(bad_request_error("run path is required"));
    }
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return Err(bad_request_error(
            "run path must be relative to the configured run directory",
        ));
    }
    if relative_path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(bad_request_error(
            "run path must stay inside the configured run directory",
        ));
    }
    let joined = root.join(relative_path);
    if joined.exists() {
        let canonical_root = root
            .canonicalize()
            .map_err(|error| internal_error(format!("failed to resolve run directory: {error}")))?;
        let canonical_joined = joined
            .canonicalize()
            .map_err(|error| internal_error(format!("failed to resolve run path: {error}")))?;
        if !canonical_joined.starts_with(&canonical_root) {
            return Err(bad_request_error(
                "run path must stay inside the configured run directory",
            ));
        }
    }
    Ok(joined)
}

fn system_time_ms(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis())
}

fn portal_timestamp_id(prefix: &str) -> String {
    let millis = system_time_ms(SystemTime::now()).unwrap_or_default();
    format!("{prefix}-{millis}")
}

fn build_run_summary(
    path: &str,
    updated_at_ms: u128,
    run: &harn_vm::orchestration::RunRecord,
) -> PortalRunSummary {
    let usage = run.usage.clone().unwrap_or_default();
    let (last_stage_node_id, failure_summary) = latest_stage_summary(run);
    PortalRunSummary {
        path: path.to_string(),
        id: run.id.clone(),
        workflow_name: run
            .workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone()),
        status: run.status.clone(),
        last_stage_node_id,
        failure_summary,
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        duration_ms: run_duration_ms(run),
        stage_count: run.stages.len(),
        child_run_count: run.child_runs.len(),
        call_count: usage.call_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        models: usage.models,
        updated_at_ms,
    }
}

fn latest_stage_summary(
    run: &harn_vm::orchestration::RunRecord,
) -> (Option<String>, Option<String>) {
    let last_stage = run.stages.last();
    let last_stage_node_id = last_stage.map(|stage| stage.node_id.clone());
    let failure_summary = run
        .stages
        .iter()
        .rev()
        .find(|stage| is_failed_status(&stage.status) || is_failed_status(&stage.outcome))
        .map(|stage| {
            let error = stage.metadata.get("error").map(compact_json).or_else(|| {
                stage
                    .attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.error.clone())
            });
            match error {
                Some(error) if !error.is_empty() => format!("{} failed: {}", stage.node_id, error),
                _ => format!("{} failed with {}", stage.node_id, stage.outcome),
            }
        });
    (last_stage_node_id, failure_summary)
}

fn summarize_runs(runs: &[PortalRunSummary]) -> PortalStats {
    let total_runs = runs.len();
    let completed_runs = runs
        .iter()
        .filter(|run| is_completed_status(&run.status))
        .count();
    let active_runs = runs
        .iter()
        .filter(|run| !is_completed_status(&run.status) && !is_failed_status(&run.status))
        .count();
    let failed_runs = runs
        .iter()
        .filter(|run| is_failed_status(&run.status))
        .count();
    let durations: Vec<u64> = runs.iter().filter_map(|run| run.duration_ms).collect();
    let avg_duration_ms = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<u64>() / durations.len() as u64
    };
    PortalStats {
        total_runs,
        completed_runs,
        active_runs,
        failed_runs,
        avg_duration_ms,
    }
}

fn build_run_detail(
    run_dir: &Path,
    relative_path: &str,
    run: &harn_vm::orchestration::RunRecord,
) -> PortalRunDetail {
    let summary = build_run_summary(relative_path, 0, run);
    let spans = build_spans(run);
    let stages = build_stages(run);
    let activities = build_activities(&spans, &stages);
    let transitions = build_transitions(run);
    let checkpoints = build_checkpoints(run);
    let artifacts = build_artifacts(run);
    let transcript_steps = discover_transcript_steps(run_dir, relative_path).unwrap_or_default();
    let story = build_story(run);
    let child_runs = run
        .child_runs
        .iter()
        .map(|child| PortalChildRun {
            worker_name: child.worker_name.clone(),
            status: child.status.clone(),
            started_at: child.started_at.clone(),
            finished_at: child.finished_at.clone(),
            run_id: child.run_id.clone(),
            run_path: child.run_path.as_ref().and_then(|path| {
                PathBuf::from(path)
                    .strip_prefix(run_dir)
                    .ok()
                    .map(|value| value.display().to_string())
                    .or_else(|| Some(path.clone()))
            }),
            task: child.task.clone(),
        })
        .collect::<Vec<_>>();
    let insights = build_insights(run, &summary, &stages, &spans, &story, &transcript_steps);

    PortalRunDetail {
        summary,
        task: run.task.clone(),
        workflow_id: run.workflow_id.clone(),
        parent_run_id: run.parent_run_id.clone(),
        root_run_id: run.root_run_id.clone(),
        policy_summary: build_policy_summary(run),
        replay_summary: build_replay_summary(run.replay_fixture.as_ref()),
        execution: run.execution.clone(),
        insights,
        stages,
        spans,
        activities,
        transitions,
        checkpoints,
        artifacts,
        execution_summary: build_execution_summary(run.execution.as_ref()),
        transcript_steps,
        story,
        child_runs,
    }
}

fn build_insights(
    run: &harn_vm::orchestration::RunRecord,
    summary: &PortalRunSummary,
    stages: &[PortalStage],
    spans: &[PortalSpan],
    story: &[PortalStorySection],
    transcript_steps: &[PortalTranscriptStep],
) -> Vec<PortalInsight> {
    let slowest_stage = stages
        .iter()
        .filter_map(|stage| stage.duration_ms.map(|duration| (stage, duration)))
        .max_by_key(|(_, duration)| *duration);
    let noisiest_kind = span_kind_totals(spans)
        .into_iter()
        .max_by_key(|(_, duration)| *duration);
    let top_story = story.first();
    let heaviest_turn = transcript_steps
        .iter()
        .max_by_key(|step| (step.total_messages, step.input_tokens.unwrap_or_default()));
    vec![
        PortalInsight {
            label: "Run shape".to_string(),
            value: format!(
                "{} stages • {} child runs",
                summary.stage_count, summary.child_run_count
            ),
            detail: format!(
                "status={} • {} transcript sections",
                run.status,
                story.len()
            ),
        },
        PortalInsight {
            label: "Slowest stage".to_string(),
            value: slowest_stage
                .map(|(stage, duration)| {
                    format!("{} ({})", stage.node_id, format_duration(duration))
                })
                .unwrap_or_else(|| "No timed stages".to_string()),
            detail: slowest_stage
                .map(|(stage, _)| {
                    format!(
                        "{} attempts • {} artifacts",
                        stage.attempt_count, stage.artifact_count
                    )
                })
                .unwrap_or_else(|| "Stage timing metadata was not available".to_string()),
        },
        PortalInsight {
            label: "Where time went".to_string(),
            value: noisiest_kind
                .as_ref()
                .map(|(kind, duration)| {
                    format!("{} ({})", humanize_kind(kind), format_duration(*duration))
                })
                .unwrap_or_else(|| "No trace spans".to_string()),
            detail: format!("{} spans captured", spans.len()),
        },
        PortalInsight {
            label: "First thing to read".to_string(),
            value: top_story
                .map(|section| section.title.clone())
                .unwrap_or_else(|| "No transcript sections".to_string()),
            detail: top_story
                .map(|section| section.preview.clone())
                .unwrap_or_else(|| {
                    "This run has trace data but no captured visible transcript".to_string()
                }),
        },
        PortalInsight {
            label: "Heaviest model turn".to_string(),
            value: heaviest_turn
                .map(|step| format!("Step {} ({})", step.call_index, step.total_messages))
                .unwrap_or_else(|| "No saved model transcript".to_string()),
            detail: heaviest_turn
                .map(|step| {
                    format!(
                        "kept {} • added {} • {}",
                        step.kept_messages, step.added_messages, step.summary
                    )
                })
                .unwrap_or_else(|| {
                    "Enable HARN_LLM_TRANSCRIPT_DIR to persist full model-turn detail".to_string()
                }),
        },
    ]
}

fn build_stages(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalStage> {
    run.stages
        .iter()
        .map(|stage| PortalStage {
            id: stage.id.clone(),
            node_id: stage.node_id.clone(),
            kind: stage.kind.clone(),
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            branch: stage.branch.clone(),
            started_at: stage.started_at.clone(),
            finished_at: stage.finished_at.clone(),
            duration_ms: stage_duration_ms(stage),
            artifact_count: stage.artifacts.len(),
            attempt_count: stage.attempts.len(),
            verification_summary: stage.verification.as_ref().map(compact_json),
            debug: build_stage_debug(stage),
        })
        .collect()
}

fn build_spans(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalSpan> {
    let mut spans = run.trace_spans.clone();
    spans.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| b.duration_ms.cmp(&a.duration_ms))
    });
    let by_id: HashMap<u64, harn_vm::orchestration::RunTraceSpanRecord> = spans
        .iter()
        .map(|span| (span.span_id, span.clone()))
        .collect();
    let mut lane_ends: Vec<u64> = Vec::new();

    spans
        .into_iter()
        .map(|span| {
            let depth = span_depth(&span, &by_id);
            let lane = match lane_ends.iter().position(|end| *end <= span.start_ms) {
                Some(index) => {
                    lane_ends[index] = span.start_ms + span.duration_ms;
                    index
                }
                None => {
                    lane_ends.push(span.start_ms + span.duration_ms);
                    lane_ends.len() - 1
                }
            };

            PortalSpan {
                span_id: span.span_id,
                parent_id: span.parent_id,
                kind: span.kind.clone(),
                name: span.name.clone(),
                start_ms: span.start_ms,
                duration_ms: span.duration_ms,
                end_ms: span.start_ms + span.duration_ms,
                label: span_label(&span),
                lane,
                depth,
                metadata: span.metadata,
            }
        })
        .collect()
}

fn build_activities(spans: &[PortalSpan], stages: &[PortalStage]) -> Vec<PortalActivity> {
    spans
        .iter()
        .filter(|span| span.kind != "pipeline")
        .map(|span| PortalActivity {
            label: span.label.clone(),
            kind: span.kind.clone(),
            started_offset_ms: span.start_ms,
            duration_ms: span.duration_ms,
            stage_node_id: owning_stage(span, stages).map(|stage| stage.node_id.clone()),
            call_id: span
                .metadata
                .get("call_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            summary: compact_metadata(&span.metadata),
        })
        .collect()
}

fn build_story(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalStorySection> {
    let mut story = Vec::new();

    if let Some(transcript) = &run.transcript {
        collect_story_sections(transcript, "Run transcript", "run", &mut story);
    }

    for stage in &run.stages {
        if let Some(transcript) = &stage.transcript {
            collect_story_sections(
                transcript,
                &format!("Stage {}", stage.node_id),
                "stage",
                &mut story,
            );
        } else if let Some(text) = &stage.visible_text {
            story.push(PortalStorySection {
                title: format!("Stage {}", stage.node_id),
                scope: "stage".to_string(),
                role: "assistant".to_string(),
                source: "visible_text".to_string(),
                preview: preview_text(text),
                text: text.clone(),
            });
        }
    }

    story
}

fn build_transitions(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalTransition> {
    run.transitions
        .iter()
        .map(|transition| PortalTransition {
            from_node_id: transition.from_node_id.clone(),
            to_node_id: transition.to_node_id.clone(),
            branch: transition.branch.clone(),
            consumed_count: transition.consumed_artifact_ids.len(),
            produced_count: transition.produced_artifact_ids.len(),
        })
        .collect()
}

fn build_checkpoints(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalCheckpoint> {
    run.checkpoints
        .iter()
        .map(|checkpoint| PortalCheckpoint {
            reason: checkpoint.reason.clone(),
            ready_count: checkpoint.ready_nodes.len(),
            completed_count: checkpoint.completed_nodes.len(),
            last_stage_id: checkpoint.last_stage_id.clone(),
        })
        .collect()
}

fn build_artifacts(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalArtifact> {
    run.artifacts
        .iter()
        .map(|artifact| PortalArtifact {
            id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            title: artifact
                .title
                .clone()
                .unwrap_or_else(|| artifact.kind.clone()),
            source: artifact.source.clone(),
            stage: artifact.stage.clone(),
            estimated_tokens: artifact.estimated_tokens,
            lineage_count: artifact.lineage.len(),
            preview: artifact
                .text
                .as_deref()
                .map(preview_text)
                .or_else(|| artifact.data.as_ref().map(compact_json))
                .unwrap_or_else(|| "No text body persisted".to_string()),
        })
        .collect()
}

fn build_execution_summary(
    execution: Option<&harn_vm::orchestration::RunExecutionRecord>,
) -> Option<PortalExecutionSummary> {
    execution.map(|execution| PortalExecutionSummary {
        cwd: execution.cwd.clone(),
        repo_path: execution.repo_path.clone(),
        worktree_path: execution.worktree_path.clone(),
        branch: execution.branch.clone(),
        adapter: execution.adapter.clone(),
    })
}

fn build_policy_summary(run: &harn_vm::orchestration::RunRecord) -> PortalPolicySummary {
    let validation = run.metadata.get("validation").cloned().and_then(|value| {
        serde_json::from_value::<harn_vm::orchestration::WorkflowValidationReport>(value).ok()
    });
    let capabilities = run
        .policy
        .capabilities
        .iter()
        .flat_map(|(capability, ops)| {
            if ops.is_empty() {
                vec![capability.clone()]
            } else {
                ops.iter()
                    .map(|op| format!("{capability}.{op}"))
                    .collect::<Vec<_>>()
            }
        })
        .collect::<Vec<_>>();
    let tool_arg_constraints = run
        .policy
        .tool_arg_constraints
        .iter()
        .map(|constraint| {
            if constraint.arg_patterns.is_empty() {
                constraint.tool.clone()
            } else {
                format!(
                    "{} → {}",
                    constraint.tool,
                    constraint.arg_patterns.join(", ")
                )
            }
        })
        .collect::<Vec<_>>();

    PortalPolicySummary {
        tools: run.policy.tools.clone(),
        capabilities,
        workspace_roots: run.policy.workspace_roots.clone(),
        side_effect_level: run.policy.side_effect_level.clone(),
        recursion_limit: run.policy.recursion_limit,
        tool_arg_constraints,
        validation_valid: validation.as_ref().map(|report| report.valid),
        validation_errors: validation
            .as_ref()
            .map(|report| report.errors.clone())
            .unwrap_or_default(),
        validation_warnings: validation
            .as_ref()
            .map(|report| report.warnings.clone())
            .unwrap_or_default(),
        reachable_nodes: validation
            .as_ref()
            .map(|report| report.reachable_nodes.clone())
            .unwrap_or_default(),
    }
}

fn build_replay_summary(
    replay: Option<&harn_vm::orchestration::ReplayFixture>,
) -> Option<PortalReplaySummary> {
    replay.map(|fixture| PortalReplaySummary {
        fixture_id: fixture.id.clone(),
        source_run_id: fixture.source_run_id.clone(),
        created_at: fixture.created_at.clone(),
        expected_status: fixture.expected_status.clone(),
        stage_assertions: fixture
            .stage_assertions
            .iter()
            .map(|stage| PortalReplayAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.expected_status.clone(),
                expected_outcome: stage.expected_outcome.clone(),
                expected_branch: stage.expected_branch.clone(),
                required_artifact_kinds: stage.required_artifact_kinds.clone(),
                visible_text_contains: stage.visible_text_contains.clone(),
            })
            .collect(),
    })
}

fn build_stage_debug(stage: &harn_vm::orchestration::RunStageRecord) -> PortalStageDebug {
    let usage = stage.usage.clone().unwrap_or_default();
    PortalStageDebug {
        call_count: usage.call_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        consumed_artifact_ids: stage.consumed_artifact_ids.clone(),
        produced_artifact_ids: stage.produced_artifact_ids.clone(),
        selected_artifact_ids: string_array_value(&stage.metadata, "selected_artifact_ids"),
        worker_id: metadata_string(&stage.metadata, "worker_id"),
        error: metadata_pretty_json(&stage.metadata, "error"),
        model_policy: metadata_pretty_json(&stage.metadata, "model_policy"),
        transcript_policy: metadata_pretty_json(&stage.metadata, "transcript_policy"),
        context_policy: metadata_pretty_json(&stage.metadata, "context_policy"),
        retry_policy: metadata_pretty_json(&stage.metadata, "retry_policy"),
        capability_policy: metadata_pretty_json(&stage.metadata, "effective_capability_policy"),
        input_contract: metadata_pretty_json(&stage.metadata, "input_contract"),
        output_contract: metadata_pretty_json(&stage.metadata, "output_contract"),
        prompt: metadata_pretty_json(&stage.metadata, "prompt"),
        system_prompt: metadata_pretty_json(&stage.metadata, "system_prompt"),
        rendered_context: metadata_pretty_json(&stage.metadata, "rendered_context"),
    }
}

fn discover_transcript_steps(
    run_dir: &Path,
    relative_path: &str,
) -> Option<Vec<PortalTranscriptStep>> {
    let run_path = run_dir.join(relative_path);
    let stem = run_path.file_stem()?.to_str()?;
    let parent = run_path.parent()?;
    let transcript_path = parent.join(format!("{stem}-llm/llm_transcript.jsonl"));
    if !transcript_path.exists() {
        return None;
    }
    parse_transcript_steps(&transcript_path).ok()
}

fn parse_transcript_steps(path: &Path) -> Result<Vec<PortalTranscriptStep>, String> {
    // Transcripts are an append-only JSONL event stream; reconstruct steps
    // by replaying system_prompt / tool_schemas / message events and
    // crystallizing one PortalTranscriptStep per provider_call_request +
    // response pair.
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut steps = Vec::<PortalTranscriptStep>::new();
    let mut by_call = HashMap::<String, usize>::new();
    let mut call_index = 0usize;
    let mut current_system_prompt: Option<String> = None;
    let mut current_schema_names: Vec<String> = Vec::new();
    let mut accumulated_messages: Vec<PortalTranscriptMessage> = Vec::new();
    let mut previous_total: usize = 0;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let raw: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let event_type = raw
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");

        match event_type {
            "system_prompt" => {
                current_system_prompt = raw
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
            "tool_schemas" => {
                current_schema_names = raw
                    .get("schemas")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                item.get("name")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
            }
            "message" => {
                accumulated_messages.push(PortalTranscriptMessage {
                    role: raw
                        .get("role")
                        .and_then(|value| value.as_str())
                        .unwrap_or("user")
                        .to_string(),
                    content: raw
                        .get("content")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
            "provider_call_request" => {
                let call_id = raw
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if call_id.is_empty() {
                    continue;
                }
                call_index += 1;
                let total_messages = accumulated_messages.len();
                let kept_messages = previous_total.min(total_messages);
                let added_context = accumulated_messages
                    .iter()
                    .skip(kept_messages)
                    .cloned()
                    .collect::<Vec<_>>();
                previous_total = total_messages;
                let step = PortalTranscriptStep {
                    call_id: call_id.clone(),
                    span_id: raw.get("span_id").and_then(|value| value.as_u64()),
                    iteration: raw
                        .get("iteration")
                        .and_then(|value| value.as_u64())
                        .unwrap_or_default() as usize,
                    call_index,
                    model: raw
                        .get("model")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string(),
                    provider: raw
                        .get("provider")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    kept_messages,
                    added_messages: added_context.len(),
                    total_messages,
                    input_tokens: None,
                    output_tokens: None,
                    system_prompt: current_system_prompt.clone(),
                    added_context,
                    response_text: None,
                    thinking: None,
                    tool_calls: current_schema_names.clone(),
                    summary: "Waiting for model response".to_string(),
                };
                by_call.insert(call_id, steps.len());
                steps.push(step);
            }
            "provider_call_response" => {
                let call_id = raw
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(index) = by_call.get(&call_id).copied() {
                    let step = &mut steps[index];
                    step.span_id = step
                        .span_id
                        .or_else(|| raw.get("span_id").and_then(|value| value.as_u64()));
                    step.input_tokens = raw.get("input_tokens").and_then(|value| value.as_i64());
                    step.output_tokens = raw.get("output_tokens").and_then(|value| value.as_i64());
                    step.response_text = raw
                        .get("text")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    step.thinking = raw
                        .get("thinking")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let response_tool_calls = raw
                        .get("tool_calls")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    item.get("name")
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !response_tool_calls.is_empty() {
                        step.tool_calls = response_tool_calls;
                    }
                    step.summary = summarize_transcript_step(step);
                }
            }
            _ => {}
        }
    }

    Ok(steps)
}

fn summarize_transcript_step(step: &PortalTranscriptStep) -> String {
    if let Some(last_tool) = step.tool_calls.last() {
        return format!(
            "kept {} messages, added {}, then asked for {}",
            step.kept_messages, step.added_messages, last_tool
        );
    }
    if step.response_text.is_some() {
        return format!(
            "kept {} messages, added {}, then replied in text",
            step.kept_messages, step.added_messages
        );
    }
    format!(
        "kept {} messages, added {}",
        step.kept_messages, step.added_messages
    )
}

fn collect_story_sections(
    value: &serde_json::Value,
    title: &str,
    scope: &str,
    out: &mut Vec<PortalStorySection>,
) {
    if let Some(events) = value.get("events").and_then(|events| events.as_array()) {
        for event in events {
            let role = event
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("assistant");
            let source = event
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("message");
            let text = extract_event_text(event);
            if text.trim().is_empty() {
                continue;
            }
            out.push(PortalStorySection {
                title: title.to_string(),
                scope: scope.to_string(),
                role: role.to_string(),
                source: source.to_string(),
                preview: preview_text(&text),
                text,
            });
        }
        return;
    }

    if let Some(entries) = value.as_array() {
        for entry in entries {
            let role = entry
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("assistant");
            let text = extract_event_text(entry);
            if text.trim().is_empty() {
                continue;
            }
            out.push(PortalStorySection {
                title: title.to_string(),
                scope: scope.to_string(),
                role: role.to_string(),
                source: "message".to_string(),
                preview: preview_text(&text),
                text,
            });
        }
    }
}

fn extract_event_text(value: &serde_json::Value) -> String {
    if let Some(text) = value.get("text").and_then(|text| text.as_str()) {
        return text.to_string();
    }
    if let Some(content) = value.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(items) = content.as_array() {
            return items
                .iter()
                .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    if let Some(blocks) = value.get("blocks").and_then(|blocks| blocks.as_array()) {
        return blocks
            .iter()
            .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

fn preview_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let line = normalized
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.len() > 180 {
        format!("{}...", &line[..180])
    } else {
        line.to_string()
    }
}

fn stage_duration_ms(stage: &harn_vm::orchestration::RunStageRecord) -> Option<u64> {
    if let Some(finished) = &stage.finished_at {
        let start = date_ms(&stage.started_at)?;
        let finish = date_ms(finished)?;
        return finish.checked_sub(start);
    }
    stage.usage.as_ref().and_then(|usage| {
        let duration = usage.total_duration_ms.max(0) as u64;
        if duration > 0 {
            Some(duration)
        } else {
            None
        }
    })
}

fn run_duration_ms(run: &harn_vm::orchestration::RunRecord) -> Option<u64> {
    if let Some(usage) = &run.usage {
        let duration = usage.total_duration_ms.max(0) as u64;
        if duration > 0 {
            return Some(duration);
        }
    }
    if let Some(max_end) = run
        .trace_spans
        .iter()
        .map(|span| span.start_ms + span.duration_ms)
        .max()
    {
        if max_end > 0 {
            return Some(max_end);
        }
    }
    let stage_total = run.stages.iter().filter_map(stage_duration_ms).sum::<u64>();
    if stage_total > 0 {
        return Some(stage_total);
    }
    if let Some(finished) = &run.finished_at {
        let start = date_ms(&run.started_at)?;
        let finish = date_ms(finished)?;
        return finish.checked_sub(start);
    }
    None
}

fn date_ms(value: &str) -> Option<u64> {
    let parsed = chrono_like_parse(value)?;
    Some(parsed)
}

fn chrono_like_parse(value: &str) -> Option<u64> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    Some(parsed.unix_timestamp_nanos() as u64 / 1_000_000)
}

fn span_depth(
    span: &harn_vm::orchestration::RunTraceSpanRecord,
    by_id: &HashMap<u64, harn_vm::orchestration::RunTraceSpanRecord>,
) -> usize {
    let mut depth = 0usize;
    let mut cursor = span.parent_id.and_then(|id| by_id.get(&id));
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = cursor {
        if !seen.insert(parent.span_id) {
            break;
        }
        depth += 1;
        cursor = parent.parent_id.and_then(|id| by_id.get(&id));
    }
    depth
}

fn span_label(span: &harn_vm::orchestration::RunTraceSpanRecord) -> String {
    match span.kind.as_str() {
        "llm_call" => span
            .metadata
            .get("model")
            .and_then(|value| value.as_str())
            .map(|model| format!("Model • {model}"))
            .unwrap_or_else(|| "Model call".to_string()),
        "tool_call" => span
            .metadata
            .get("tool_name")
            .and_then(|value| value.as_str())
            .map(|tool| format!("Tool • {tool}"))
            .unwrap_or_else(|| format!("Tool • {}", span.name)),
        _ => span.name.replace('_', " "),
    }
}

fn compact_metadata(metadata: &BTreeMap<String, serde_json::Value>) -> String {
    if metadata.is_empty() {
        return "No extra metadata".to_string();
    }
    let sample = metadata
        .iter()
        .take(3)
        .map(|(key, value)| format!("{key}={}", compact_json(value)))
        .collect::<Vec<_>>()
        .join(" • ");
    if metadata.len() > 3 {
        format!("{sample} • +{} more", metadata.len() - 3)
    } else {
        sample
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.to_string(),
        serde_json::Value::Array(values) => format!("{} items", values.len()),
        serde_json::Value::Object(values) => format!("{} fields", values.len()),
    }
}

fn pretty_json(value: &serde_json::Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| compact_json(value))
}

fn metadata_pretty_json(
    metadata: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    metadata.get(key).map(pretty_json)
}

fn metadata_string(metadata: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn string_array_value(metadata: &BTreeMap<String, serde_json::Value>, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn span_kind_totals(spans: &[PortalSpan]) -> Vec<(String, u64)> {
    let mut totals = HashMap::<String, u64>::new();
    for span in spans {
        *totals.entry(span.kind.clone()).or_default() += span.duration_ms;
    }
    let mut values = totals.into_iter().collect::<Vec<_>>();
    values.sort_by(|a, b| b.1.cmp(&a.1));
    values
}

fn humanize_kind(kind: &str) -> String {
    kind.replace('_', " ")
}

fn owning_stage<'a>(span: &PortalSpan, stages: &'a [PortalStage]) -> Option<&'a PortalStage> {
    let offsets = stages
        .iter()
        .filter_map(|stage| stage.duration_ms.map(|duration| (stage, duration)));
    let mut cursor = 0u64;
    for (stage, duration) in offsets {
        let start = cursor;
        let end = cursor + duration;
        if span.start_ms >= start && span.end_ms <= end {
            return Some(stage);
        }
        cursor = end;
    }
    None
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms >= 60_000 {
        format!("{:.1}m", duration_ms as f64 / 60_000.0)
    } else if duration_ms >= 1_000 {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

fn is_completed_status(status: &str) -> bool {
    matches!(status, "complete" | "completed" | "success" | "verified")
}

fn is_failed_status(status: &str) -> bool {
    matches!(status, "failed" | "error" | "cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_portal_state(run_dir: &Path) -> Arc<PortalState> {
        Arc::new(PortalState {
            run_dir: run_dir.to_path_buf(),
            workspace_root: run_dir.to_path_buf(),
            launch_program: PathBuf::from("harn"),
            launch_jobs: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    #[test]
    fn resolve_run_path_rejects_parent_segments() {
        let temp = tempfile::tempdir().unwrap();
        let error = resolve_run_path(temp.path(), "../outside.json").unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn scan_runs_ignores_non_run_json() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("ignore.json"), "{not valid json").unwrap();
        fs::write(
            temp.path().join("launch.json"),
            serde_json::json!({
                "mode": "playground",
                "task": "hello"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            temp.path().join("run.json"),
            serde_json::json!({
                "_type": "run_record",
                "id": "run-1",
                "workflow_id": "wf",
                "workflow_name": "demo",
                "task": "task",
                "status": "complete",
                "started_at": "2026-04-03T01:00:00Z",
                "finished_at": "2026-04-03T01:00:02Z",
                "stages": [],
                "transitions": [],
                "checkpoints": [],
                "pending_nodes": [],
                "completed_nodes": [],
                "child_runs": [],
                "artifacts": [],
                "policy": {},
                "metadata": {}
            })
            .to_string(),
        )
        .unwrap();

        let runs = scan_runs(temp.path()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].workflow_name, "demo");
    }

    #[test]
    fn build_run_summary_includes_failure_context() {
        let run = harn_vm::orchestration::RunRecord {
            id: "run-1".to_string(),
            workflow_id: "wf".to_string(),
            workflow_name: Some("demo".to_string()),
            status: "failed".to_string(),
            started_at: "2026-04-03T01:00:00Z".to_string(),
            stages: vec![harn_vm::orchestration::RunStageRecord {
                id: "stage-1".to_string(),
                node_id: "verify".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                started_at: "2026-04-03T01:00:00Z".to_string(),
                attempts: vec![harn_vm::orchestration::RunStageAttemptRecord {
                    error: Some("assertion failed".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let summary = build_run_summary("run.json", 0, &run);
        assert_eq!(summary.last_stage_node_id.as_deref(), Some("verify"));
        assert_eq!(
            summary.failure_summary.as_deref(),
            Some("verify failed: assertion failed")
        );
    }

    #[test]
    fn scan_launch_targets_finds_harn_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("examples")).unwrap();
        fs::create_dir_all(temp.path().join("conformance/tests")).unwrap();
        fs::write(temp.path().join("examples/demo.harn"), "pipeline main() {}").unwrap();
        fs::write(
            temp.path().join("conformance/tests/check.harn"),
            "pipeline main() {}",
        )
        .unwrap();

        let targets = scan_launch_targets(temp.path()).unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets
            .iter()
            .any(|target| target.path == "examples/demo.harn"));
        assert!(targets
            .iter()
            .any(|target| target.path == "conformance/tests/check.harn"));
    }

    #[test]
    fn validate_launch_request_requires_exactly_one_mode() {
        let missing = PortalLaunchRequest {
            file_path: None,
            source: None,
            task: None,
            provider: None,
            model: None,
            env: None,
        };
        assert!(validate_launch_request(&missing).is_err());

        let conflicting = PortalLaunchRequest {
            file_path: Some("examples/demo.harn".to_string()),
            source: Some("pipeline main() {}".to_string()),
            task: None,
            provider: None,
            model: None,
            env: None,
        };
        assert!(validate_launch_request(&conflicting).is_err());
    }

    #[test]
    fn validated_env_overrides_rejects_non_shell_style_names() {
        let env = BTreeMap::from([
            ("OPENAI_API_KEY".to_string(), "secret".to_string()),
            ("bad-key".to_string(), "oops".to_string()),
        ]);
        assert!(validated_env_overrides(Some(&env)).is_err());
    }

    #[test]
    fn build_launch_env_sets_transcript_dir_inside_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let env = build_launch_env(Some(temp.path()), &BTreeMap::new());
        assert_eq!(
            env.get("HARN_LLM_TRANSCRIPT_DIR").map(String::as_str),
            Some(temp.path().join("run-llm").to_str().unwrap())
        );
    }

    #[test]
    fn materialize_playground_target_creates_workspace_files() {
        let temp = tempfile::tempdir().unwrap();
        let target = materialize_launch_target(
            temp.path(),
            temp.path(),
            "job-1",
            PortalLaunchRequest {
                file_path: None,
                source: None,
                task: Some("hello world".to_string()),
                provider: Some("mock".to_string()),
                model: Some("mock".to_string()),
                env: None,
            },
        )
        .unwrap();

        let workspace_dir = target.workspace_dir.expect("workspace dir");
        assert!(workspace_dir.join("workflow.harn").exists());
        assert!(workspace_dir.join("task.txt").exists());
        assert!(workspace_dir.join("launch.json").exists());
        let source = fs::read_to_string(workspace_dir.join("workflow.harn")).unwrap();
        assert!(source.contains("workspace_file"));
        assert!(source.contains("persist_path"));
    }

    #[tokio::test]
    async fn api_runs_returns_json() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("run.json"),
            serde_json::json!({
                "_type": "run_record",
                "id": "run-1",
                "workflow_id": "wf",
                "workflow_name": "demo",
                "task": "task",
                "status": "complete",
                "started_at": "2026-04-03T01:00:00Z",
                "finished_at": "2026-04-03T01:00:02Z",
                "stages": [],
                "transitions": [],
                "checkpoints": [],
                "pending_nodes": [],
                "completed_nodes": [],
                "child_runs": [],
                "artifacts": [],
                "policy": {},
                "metadata": {}
            })
            .to_string(),
        )
        .unwrap();

        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn filter_and_sort_runs_applies_search_status_and_ordering() {
        let runs = vec![
            PortalRunSummary {
                path: "alpha.json".to_string(),
                id: "run-alpha".to_string(),
                workflow_name: "alpha".to_string(),
                status: "completed".to_string(),
                last_stage_node_id: Some("finalize".to_string()),
                failure_summary: None,
                started_at: "2026-04-04T10:00:00Z".to_string(),
                finished_at: None,
                duration_ms: Some(100),
                stage_count: 1,
                child_run_count: 0,
                call_count: 1,
                input_tokens: 10,
                output_tokens: 5,
                models: vec!["gpt-4o".to_string()],
                updated_at_ms: 1,
            },
            PortalRunSummary {
                path: "beta.json".to_string(),
                id: "run-beta".to_string(),
                workflow_name: "beta".to_string(),
                status: "failed".to_string(),
                last_stage_node_id: Some("verify".to_string()),
                failure_summary: Some("assertion failed".to_string()),
                started_at: "2026-04-04T11:00:00Z".to_string(),
                finished_at: None,
                duration_ms: Some(200),
                stage_count: 2,
                child_run_count: 0,
                call_count: 2,
                input_tokens: 20,
                output_tokens: 10,
                models: vec!["qwen".to_string()],
                updated_at_ms: 2,
            },
        ];

        let query = ListRunsQuery {
            q: Some("assertion".to_string()),
            workflow: None,
            status: Some("failed".to_string()),
            sort: Some("duration".to_string()),
            page: Some(1),
            page_size: Some(25),
        };

        let filtered = filter_and_sort_runs(runs, &query);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "beta.json");
    }

    #[tokio::test]
    async fn api_meta_returns_workspace_and_run_dir() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/meta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_highlight_keywords_returns_payload() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/highlight/keywords")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_llm_options_returns_payload() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/llm/options")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn portal_index_and_assets_are_served() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));

        let index_response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(index_response.status(), StatusCode::OK);

        let asset_response = app
            .oneshot(
                Request::builder()
                    .uri("/assets/portal/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(asset_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_run_rejects_escaping_paths() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/run?path=../outside.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_run_returns_not_found_for_missing_runs() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/run?path=missing.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_compare_returns_stage_diffs() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("left.json"),
            serde_json::json!({
                "_type": "run_record",
                "id": "run-left",
                "workflow_id": "wf",
                "workflow_name": "demo",
                "task": "task",
                "status": "completed",
                "started_at": "2026-04-03T01:00:00Z",
                "finished_at": "2026-04-03T01:00:02Z",
                "stages": [{
                    "id": "stage-1",
                    "node_id": "plan",
                    "status": "completed",
                    "outcome": "success",
                    "started_at": "2026-04-03T01:00:00Z",
                    "finished_at": "2026-04-03T01:00:01Z",
                    "artifacts": []
                }],
                "transitions": [],
                "checkpoints": [],
                "pending_nodes": [],
                "completed_nodes": ["plan"],
                "child_runs": [],
                "artifacts": [],
                "policy": {},
                "metadata": {}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            temp.path().join("right.json"),
            serde_json::json!({
                "_type": "run_record",
                "id": "run-right",
                "workflow_id": "wf",
                "workflow_name": "demo",
                "task": "task",
                "status": "failed",
                "started_at": "2026-04-03T01:01:00Z",
                "finished_at": "2026-04-03T01:01:03Z",
                "stages": [{
                    "id": "stage-1",
                    "node_id": "plan",
                    "status": "failed",
                    "outcome": "error",
                    "started_at": "2026-04-03T01:01:00Z",
                    "finished_at": "2026-04-03T01:01:02Z",
                    "artifacts": [{"id":"artifact-1","kind":"artifact","created_at":"2026-04-03T01:01:02Z"}]
                }],
                "transitions": [{"id":"transition-1","to_node_id":"plan","timestamp":"2026-04-03T01:01:02Z"}],
                "checkpoints": [{"id":"checkpoint-1","reason":"error","persisted_at":"2026-04-03T01:01:02Z"}],
                "pending_nodes": [],
                "completed_nodes": [],
                "child_runs": [],
                "artifacts": [{"id":"artifact-1","kind":"artifact","created_at":"2026-04-03T01:01:02Z"}],
                "policy": {},
                "metadata": {}
            })
            .to_string(),
        )
        .unwrap();

        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/compare?left=left.json&right=right.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let diff: PortalRunDiff = serde_json::from_slice(&body).unwrap();
        assert!(diff.status_changed);
        assert_eq!(diff.left_status, "completed");
        assert_eq!(diff.right_status, "failed");
        assert!(!diff.stage_diffs.is_empty());
        assert_eq!(diff.transition_count_delta, 1);
        assert_eq!(diff.artifact_count_delta, 1);
        assert_eq!(diff.checkpoint_count_delta, 1);
    }

    #[tokio::test]
    async fn api_compare_rejects_escaping_paths() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/compare?left=../left.json&right=right.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_compare_returns_not_found_for_missing_runs() {
        let temp = tempfile::tempdir().unwrap();
        let app = build_router(test_portal_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/compare?left=left.json&right=right.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn discover_transcript_steps_reads_sibling_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        fs::write(&run_path, "{}").unwrap();
        let llm_dir = temp.path().join("run-llm");
        fs::create_dir_all(&llm_dir).unwrap();
        // Event-stream shape: system_prompt + tool_schemas once, then a
        // user message, then provider_call_request / response. Parser
        // reconstructs a PortalTranscriptStep by replaying events.
        fs::write(
            llm_dir.join("llm_transcript.jsonl"),
            concat!(
                "{\"type\":\"system_prompt\",\"content\":\"Be helpful\",\"hash\":1}\n",
                "{\"type\":\"tool_schemas\",\"schemas\":[{\"name\":\"read\"}],\"hash\":2}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Do X\",\"iteration\":1}\n",
                "{\"type\":\"provider_call_request\",\"call_id\":\"call-1\",\"iteration\":1,\"model\":\"mock\"}\n",
                "{\"type\":\"provider_call_response\",\"call_id\":\"call-1\",\"iteration\":1,\"model\":\"mock\",\"text\":\"Done\",\"input_tokens\":10,\"output_tokens\":4,\"tool_calls\":[{\"name\":\"read\"}]}\n"
            ),
        )
        .unwrap();

        let steps = discover_transcript_steps(temp.path(), "run.json").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].tool_calls, vec!["read".to_string()]);
        assert_eq!(steps[0].added_messages, 1);
        assert_eq!(steps[0].response_text.as_deref(), Some("Done"));
        assert_eq!(steps[0].system_prompt.as_deref(), Some("Be helpful"));
    }

    #[test]
    fn build_policy_summary_reads_validation_metadata() {
        let run = harn_vm::orchestration::RunRecord {
            policy: harn_vm::orchestration::CapabilityPolicy {
                tools: vec!["read".to_string(), "exec".to_string()],
                capabilities: BTreeMap::from([(
                    "workspace".to_string(),
                    vec!["read_text".to_string(), "list".to_string()],
                )]),
                workspace_roots: vec!["/tmp/project".to_string()],
                side_effect_level: Some("workspace_write".to_string()),
                recursion_limit: Some(4),
                tool_arg_constraints: vec![harn_vm::orchestration::ToolArgConstraint {
                    tool: "read".to_string(),
                    arg_patterns: vec!["src/*".to_string()],
                    arg_key: Some("path".to_string()),
                }],
                tool_annotations: BTreeMap::new(),
            },
            metadata: BTreeMap::from([(
                "validation".to_string(),
                serde_json::json!({
                    "valid": false,
                    "errors": ["missing edge"],
                    "warnings": ["unused node"],
                    "reachable_nodes": ["plan"],
                }),
            )]),
            ..Default::default()
        };

        let summary = build_policy_summary(&run);

        assert_eq!(summary.tools, vec!["read".to_string(), "exec".to_string()]);
        assert!(summary
            .capabilities
            .contains(&"workspace.read_text".to_string()));
        assert_eq!(summary.validation_valid, Some(false));
        assert_eq!(summary.validation_errors, vec!["missing edge".to_string()]);
        assert_eq!(summary.validation_warnings, vec!["unused node".to_string()]);
        assert_eq!(summary.reachable_nodes, vec!["plan".to_string()]);
    }

    #[test]
    fn build_replay_summary_reads_fixture_metadata() {
        let fixture = harn_vm::orchestration::ReplayFixture {
            id: "fixture-1".to_string(),
            source_run_id: "run-1".to_string(),
            created_at: "2026-04-04T00:00:00Z".to_string(),
            expected_status: "completed".to_string(),
            stage_assertions: vec![harn_vm::orchestration::ReplayStageAssertion {
                node_id: "plan".to_string(),
                expected_status: "completed".to_string(),
                expected_outcome: "success".to_string(),
                expected_branch: Some("true".to_string()),
                required_artifact_kinds: vec!["notes".to_string()],
                visible_text_contains: Some("done".to_string()),
            }],
            ..Default::default()
        };

        let summary = build_replay_summary(Some(&fixture)).unwrap();
        assert_eq!(summary.fixture_id, "fixture-1");
        assert_eq!(summary.stage_assertions.len(), 1);
        assert_eq!(summary.stage_assertions[0].node_id, "plan");
    }
}
