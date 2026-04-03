use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

#[derive(Clone)]
struct PortalState {
    run_dir: PathBuf,
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
    status: String,
    outcome: String,
    started_at: String,
    finished_at: Option<String>,
    duration_ms: Option<u64>,
    artifact_count: usize,
    attempt_count: usize,
    verification_summary: Option<String>,
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
    runs: Vec<PortalRunSummary>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
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

pub(crate) async fn run_portal(dir: &str, host: &str, port: u16, open_browser: bool) {
    let run_dir = PathBuf::from(dir);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| panic!("invalid portal bind address: {host}:{port}"));

    let state = Arc::new(PortalState {
        run_dir: run_dir.clone(),
    });
    let app = build_router(state);
    let url = format!("http://{addr}");

    println!("Harn portal listening on {url}");
    println!("Watching run records in {}", run_dir.display());

    if open_browser {
        let _ = webbrowser::open(&url);
    }

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|error| panic!("failed to bind portal listener: {error}"));
    axum::serve(listener, app)
        .await
        .unwrap_or_else(|error| panic!("portal server failed: {error}"));
}

fn build_router(state: Arc<PortalState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/styles.css", get(styles))
        .route("/app.js", get(app_js))
        .route("/api/runs", get(list_runs_handler))
        .route("/api/run", get(run_detail_handler))
        .route("/api/compare", get(compare_runs_handler))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../../portal/index.html"))
}

async fn styles() -> impl IntoResponse {
    asset_response(
        include_str!("../../../portal/styles.css"),
        "text/css; charset=utf-8",
    )
}

async fn app_js() -> impl IntoResponse {
    asset_response(
        include_str!("../../../portal/app.js"),
        "application/javascript; charset=utf-8",
    )
}

async fn list_runs_handler(
    State(state): State<Arc<PortalState>>,
) -> Result<Json<PortalListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let runs = scan_runs(&state.run_dir).map_err(internal_error)?;
    let stats = summarize_runs(&runs);
    Ok(Json(PortalListResponse { stats, runs }))
}

async fn run_detail_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<RunQuery>,
) -> Result<Json<PortalRunDetail>, (StatusCode, Json<ErrorResponse>)> {
    let path = safe_join(&state.run_dir, &query.path)
        .ok_or_else(|| internal_error("requested run path escapes the configured run directory"))?;
    let run = harn_vm::orchestration::load_run_record(&path)
        .map_err(|error| internal_error(format!("failed to load run record: {error}")))?;
    Ok(Json(build_run_detail(&state.run_dir, &query.path, &run)))
}

async fn compare_runs_handler(
    State(state): State<Arc<PortalState>>,
    Query(query): Query<CompareQuery>,
) -> Result<Json<PortalRunDiff>, (StatusCode, Json<ErrorResponse>)> {
    let left_path = safe_join(&state.run_dir, &query.left)
        .ok_or_else(|| internal_error("left run path escapes the configured run directory"))?;
    let right_path = safe_join(&state.run_dir, &query.right)
        .ok_or_else(|| internal_error("right run path escapes the configured run directory"))?;
    let left = harn_vm::orchestration::load_run_record(&left_path)
        .map_err(|error| internal_error(format!("failed to load left run: {error}")))?;
    let right = harn_vm::orchestration::load_run_record(&right_path)
        .map_err(|error| internal_error(format!("failed to load right run: {error}")))?;
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

fn asset_response(body: &'static str, content_type: &'static str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        content_type.parse().expect("content type"),
    );
    (headers, body).into_response()
}

fn internal_error(message: impl ToString) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

fn scan_runs(run_dir: &Path) -> Result<Vec<PortalRunSummary>, String> {
    let mut files = Vec::new();
    collect_run_files(run_dir, run_dir, &mut files)?;

    let mut runs = Vec::new();
    for (path, modified_at_ms) in files {
        if let Ok(run) = harn_vm::orchestration::load_run_record(&path) {
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

fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let joined = root.join(relative);
    let canonical_root = root.canonicalize().ok()?;
    let canonical_joined = joined.canonicalize().ok()?;
    if canonical_joined.starts_with(&canonical_root) {
        Some(canonical_joined)
    } else {
        None
    }
}

fn system_time_ms(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis())
}

fn build_run_summary(
    path: &str,
    updated_at_ms: u128,
    run: &harn_vm::orchestration::RunRecord,
) -> PortalRunSummary {
    let usage = run.usage.clone().unwrap_or_default();
    PortalRunSummary {
        path: path.to_string(),
        id: run.id.clone(),
        workflow_name: run
            .workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone()),
        status: run.status.clone(),
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
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            started_at: stage.started_at.clone(),
            finished_at: stage.finished_at.clone(),
            duration_ms: stage_duration_ms(stage),
            artifact_count: stage.artifacts.len(),
            attempt_count: stage.attempts.len(),
            verification_summary: stage.verification.as_ref().map(compact_json),
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
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut steps = Vec::<PortalTranscriptStep>::new();
    let mut by_call = HashMap::<String, usize>::new();
    let mut previous_messages: Vec<PortalTranscriptMessage> = Vec::new();
    let mut call_index = 0usize;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let raw: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let call_id = raw
            .get("call_id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        if call_id.is_empty() {
            continue;
        }
        match raw
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("")
        {
            "request" => {
                call_index += 1;
                let messages = raw
                    .get("messages")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| PortalTranscriptMessage {
                                role: item
                                    .get("role")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("user")
                                    .to_string(),
                                content: item
                                    .get("content")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let kept_messages = shared_prefix_count(&previous_messages, &messages);
                let added_context = messages
                    .iter()
                    .skip(kept_messages)
                    .cloned()
                    .collect::<Vec<_>>();
                previous_messages = messages.clone();
                let tool_calls = raw
                    .get("tool_schemas")
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
                    total_messages: messages.len(),
                    input_tokens: None,
                    output_tokens: None,
                    system_prompt: raw
                        .get("system")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    added_context,
                    response_text: None,
                    thinking: None,
                    tool_calls,
                    summary: "Waiting for model response".to_string(),
                };
                by_call.insert(call_id, steps.len());
                steps.push(step);
            }
            "response" => {
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

fn shared_prefix_count(
    previous: &[PortalTranscriptMessage],
    current: &[PortalTranscriptMessage],
) -> usize {
    let max = previous.len().min(current.len());
    let mut count = 0usize;
    while count < max {
        if previous[count].role != current[count].role
            || previous[count].content != current[count].content
        {
            break;
        }
        count += 1;
    }
    count
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

    #[test]
    fn scan_runs_ignores_non_run_json() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("ignore.json"), "{not valid json").unwrap();
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

        let app = build_router(Arc::new(PortalState {
            run_dir: temp.path().to_path_buf(),
        }));
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

        let app = build_router(Arc::new(PortalState {
            run_dir: temp.path().to_path_buf(),
        }));
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

    #[test]
    fn discover_transcript_steps_reads_sibling_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        fs::write(&run_path, "{}").unwrap();
        let llm_dir = temp.path().join("run-llm");
        fs::create_dir_all(&llm_dir).unwrap();
        fs::write(
            llm_dir.join("llm_transcript.jsonl"),
            concat!(
                "{\"type\":\"request\",\"call_id\":\"call-1\",\"iteration\":0,\"model\":\"mock\",\"messages\":[{\"role\":\"user\",\"content\":\"Do X\"}],\"system\":\"Be helpful\"}\n",
                "{\"type\":\"response\",\"call_id\":\"call-1\",\"iteration\":0,\"model\":\"mock\",\"text\":\"Done\",\"input_tokens\":10,\"output_tokens\":4,\"tool_calls\":[{\"name\":\"read\"}]}\n"
            ),
        )
        .unwrap();

        let steps = discover_transcript_steps(temp.path(), "run.json").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].tool_calls, vec!["read".to_string()]);
        assert_eq!(steps[0].added_messages, 1);
        assert_eq!(steps[0].response_text.as_deref(), Some("Done"));
    }
}
