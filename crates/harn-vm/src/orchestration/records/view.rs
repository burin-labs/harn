use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::event_log::{AnyEventLog, EventId, EventLog, LogError};
use crate::provenance::event_record_hash_from_headers;
use crate::redact::{current_policy, RedactionPolicy};

use super::super::ArtifactRecord;
use super::{
    LlmUsageRecord, RunCheckpointRecord, RunChildRecord, RunHitlQuestionRecord, RunRecord,
    RunStageRecord, RunTraceSpanRecord,
};

pub const RUN_VIEW_SCHEMA: &str = "harn.run_view.v1";
pub const SESSION_VIEW_SCHEMA: &str = "harn.session_view.v1";
pub const RUN_VIEW_SCHEMA_VERSION: u32 = 1;
pub const SESSION_VIEW_SCHEMA_VERSION: u32 = 1;
pub const SESSION_VIEW_QUERY_METHOD: &str = "harn.session_view.query";

const TEXT_LIMIT: usize = 16 * 1024;
const PREVIEW_LIMIT: usize = 1200;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ViewProducer {
    pub name: String,
    pub version: String,
}

impl Default for ViewProducer {
    fn default() -> Self {
        Self {
            name: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProjectionInfo {
    pub projection_id: String,
    pub projection_hash: Option<String>,
    pub prefix_hash: Option<String>,
    pub last_event_id: Option<EventId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunView {
    pub schema: String,
    pub schema_version: u32,
    pub producer: ViewProducer,
    pub run: RunViewRun,
    pub projection: ProjectionInfo,
    pub visible_text: Option<String>,
    pub transcript: TranscriptSummary,
    pub usage: RunViewUsage,
    pub providers: Vec<RunViewProvider>,
    pub stages: Vec<RunViewStage>,
    pub artifacts: Vec<RunViewArtifact>,
    pub checkpoints: Vec<RunViewCheckpoint>,
    pub pending: RunViewPendingState,
    pub failure: Option<RunViewFailure>,
    pub metadata: RunViewMetadata,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewRun {
    pub run_id: String,
    pub session_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub root_run_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub child_runs: Vec<RunViewChild>,
    pub run_path: Option<String>,
    pub status: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewChild {
    pub worker_id: String,
    pub worker_name: String,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub run_path: Option<String>,
    pub status: String,
    pub task: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunViewUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_duration_ms: i64,
    pub call_count: i64,
    pub total_cost: f64,
    pub models: Vec<String>,
}

impl From<&LlmUsageRecord> for RunViewUsage {
    fn from(value: &LlmUsageRecord) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_duration_ms: value.total_duration_ms,
            call_count: value.call_count,
            total_cost: value.total_cost,
            models: value.models.clone(),
        }
    }
}

impl RunViewUsage {
    fn add_usage(&mut self, usage: &RunViewUsage) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.total_duration_ms += usage.total_duration_ms;
        self.call_count += usage.call_count;
        self.total_cost += usage.total_cost;
        for model in &usage.models {
            if !model.is_empty() && !self.models.contains(model) {
                self.models.push(model.clone());
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunViewProvider {
    pub provider: String,
    pub model: String,
    pub call_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunViewStage {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub visible_text: Option<String>,
    pub usage: RunViewUsage,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub artifact_refs: Vec<String>,
    pub attempt_count: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewArtifact {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub stage: Option<String>,
    pub estimated_tokens: Option<usize>,
    pub lineage: Vec<String>,
    pub preview: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewCheckpoint {
    pub id: String,
    pub reason: String,
    pub ready_count: usize,
    pub completed_count: usize,
    pub last_stage_id: Option<String>,
    pub persisted_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewPendingState {
    pub nodes: Vec<String>,
    pub approvals: Vec<RunViewApproval>,
    pub auth: Vec<RunViewAuth>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewApproval {
    pub request_id: String,
    pub prompt: String,
    pub agent: String,
    pub trace_id: Option<String>,
    pub asked_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewAuth {
    pub provider: Option<String>,
    pub server: Option<String>,
    pub scope: Option<String>,
    pub stage_id: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewFailure {
    pub stage_id: Option<String>,
    pub node_id: Option<String>,
    pub status: String,
    pub outcome: String,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptSummary {
    pub present: bool,
    pub message_count: usize,
    pub event_count: usize,
    pub summary: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunViewMetadata {
    pub record_type: String,
    pub stage_count: usize,
    pub transition_count: usize,
    pub artifact_count: usize,
    pub checkpoint_count: usize,
    pub child_run_count: usize,
    pub observability_present: bool,
    pub planner_round_count: usize,
    pub tool_recording_count: usize,
    pub replay_fixture_id: Option<String>,
    pub execution: Option<super::RunExecutionRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionView {
    pub schema: String,
    pub schema_version: u32,
    pub producer: ViewProducer,
    pub session: SessionViewSession,
    pub projection: ProjectionInfo,
    pub runs: Vec<RunView>,
    pub history: Vec<SessionViewHistoryItem>,
    pub usage: RunViewUsage,
    pub pending: RunViewPendingState,
    pub failure: Option<RunViewFailure>,
    pub metadata: SessionViewMetadata,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionViewSession {
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub status: String,
    pub run_count: usize,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_event_id: Option<EventId>,
    pub chain_root_hash: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionViewHistoryItem {
    pub run_id: String,
    pub run_path: Option<String>,
    pub session_id: Option<String>,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub last_event_id: Option<EventId>,
    pub visible_text: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionViewMetadata {
    pub record_count: usize,
    pub event_count: usize,
    pub has_event_log: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RunViewOptions {
    pub run_path: Option<String>,
    pub last_event_id: Option<EventId>,
    pub prefix_hash: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionViewOptions {
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_event_id: Option<EventId>,
    pub chain_root_hash: Option<String>,
    pub event_count: usize,
    pub has_event_log: bool,
}

#[derive(Debug)]
pub enum RunViewError {
    EventLog(LogError),
}

impl std::fmt::Display for RunViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLog(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RunViewError {}

impl From<LogError> for RunViewError {
    fn from(error: LogError) -> Self {
        Self::EventLog(error)
    }
}

pub fn build_run_view(run: &RunRecord) -> RunView {
    build_run_view_with_options(run, RunViewOptions::default())
}

pub fn build_run_view_with_path(run: &RunRecord, run_path: Option<impl Into<String>>) -> RunView {
    build_run_view_with_options(
        run,
        RunViewOptions {
            run_path: run_path.map(Into::into),
            ..RunViewOptions::default()
        },
    )
}

pub async fn build_run_view_with_event_log(
    run: &RunRecord,
    run_path: Option<impl Into<String>>,
    log: Option<&AnyEventLog>,
) -> Result<RunView, RunViewError> {
    let mut options = RunViewOptions {
        run_path: run_path.map(Into::into),
        ..RunViewOptions::default()
    };
    if let Some(log) = log {
        if let Some(session_id) = infer_run_session_id(run) {
            let (last_event_id, prefix_hash) = read_session_tip(log, &session_id).await?;
            options.last_event_id = last_event_id;
            options.prefix_hash = prefix_hash;
        }
    }
    Ok(build_run_view_with_options(run, options))
}

pub fn build_run_view_with_options(run: &RunRecord, options: RunViewOptions) -> RunView {
    let policy = current_policy();
    let session_id = infer_run_session_id(run);
    let parent_session_id = infer_parent_session_id(run);
    let stages = run
        .stages
        .iter()
        .map(|stage| build_stage_view(stage, &policy))
        .collect::<Vec<_>>();
    let visible_text = bounded_join(
        run.stages
            .iter()
            .filter_map(|stage| stage.visible_text.as_deref())
            .map(|text| redact_bounded(text, &policy, TEXT_LIMIT)),
        TEXT_LIMIT,
    );
    let usage = run
        .usage
        .as_ref()
        .map(RunViewUsage::from)
        .unwrap_or_else(|| usage_from_stages(&stages));
    let mut view = RunView {
        schema: RUN_VIEW_SCHEMA.to_string(),
        schema_version: RUN_VIEW_SCHEMA_VERSION,
        producer: ViewProducer::default(),
        run: RunViewRun {
            run_id: run.id.clone(),
            session_id,
            parent_run_id: run.parent_run_id.clone(),
            root_run_id: run.root_run_id.clone(),
            parent_session_id,
            child_runs: run
                .child_runs
                .iter()
                .map(|child| build_child_view(child, &policy))
                .collect(),
            run_path: options
                .run_path
                .clone()
                .or_else(|| run.persisted_path.clone()),
            status: run.status.clone(),
            workflow_id: run.workflow_id.clone(),
            workflow_name: run.workflow_name.clone(),
            task: redact_bounded(&run.task, &policy, TEXT_LIMIT),
            started_at: run.started_at.clone(),
            finished_at: run.finished_at.clone(),
            duration_ms: run_duration_ms(run),
        },
        projection: ProjectionInfo {
            projection_id: String::new(),
            projection_hash: None,
            prefix_hash: options.prefix_hash,
            last_event_id: options.last_event_id,
        },
        visible_text,
        transcript: transcript_summary(run.transcript.as_ref(), &policy),
        usage,
        providers: provider_summary(run),
        stages,
        artifacts: run
            .artifacts
            .iter()
            .map(|artifact| build_artifact_view(artifact, &policy))
            .collect(),
        checkpoints: run.checkpoints.iter().map(build_checkpoint_view).collect(),
        pending: RunViewPendingState {
            nodes: run.pending_nodes.clone(),
            approvals: run
                .hitl_questions
                .iter()
                .map(|question| build_approval_view(question, &policy))
                .collect(),
            auth: pending_auth(run, &policy),
        },
        failure: failure_summary(run, &policy),
        metadata: RunViewMetadata {
            record_type: run.type_name.clone(),
            stage_count: run.stages.len(),
            transition_count: run.transitions.len(),
            artifact_count: run.artifacts.len(),
            checkpoint_count: run.checkpoints.len(),
            child_run_count: run.child_runs.len(),
            observability_present: run.observability.is_some(),
            planner_round_count: run
                .observability
                .as_ref()
                .map(|observability| observability.planner_rounds.len())
                .unwrap_or_default(),
            tool_recording_count: run.tool_recordings.len(),
            replay_fixture_id: run
                .replay_fixture
                .as_ref()
                .map(|fixture| fixture.id.clone()),
            execution: run.execution.clone(),
        },
    };
    finalize_run_projection(&mut view);
    view
}

pub fn build_session_view_from_run_views(
    runs: Vec<RunView>,
    options: SessionViewOptions,
) -> SessionView {
    let session_id = options
        .session_id
        .clone()
        .or_else(|| runs.iter().find_map(|run| run.run.session_id.clone()));
    let mut usage = RunViewUsage::default();
    let mut pending = RunViewPendingState::default();
    let mut failure = None;
    let mut started_at = options.started_at.clone();
    let mut updated_at = options.updated_at.clone();
    let history = runs
        .iter()
        .map(|run| {
            usage.add_usage(&run.usage);
            pending.nodes.extend(run.pending.nodes.clone());
            pending.approvals.extend(run.pending.approvals.clone());
            pending.auth.extend(run.pending.auth.clone());
            if failure.is_none() {
                failure = run.failure.clone();
            }
            if !run.run.started_at.is_empty() {
                started_at = min_opt_string(started_at.take(), Some(run.run.started_at.clone()));
                updated_at = max_opt_string(updated_at.take(), Some(run.run.started_at.clone()));
            }
            updated_at = max_opt_string(updated_at.take(), run.run.finished_at.clone());
            SessionViewHistoryItem {
                run_id: run.run.run_id.clone(),
                run_path: run.run.run_path.clone(),
                session_id: run.run.session_id.clone(),
                status: run.run.status.clone(),
                started_at: non_empty_string(&run.run.started_at),
                finished_at: run.run.finished_at.clone(),
                last_event_id: run.projection.last_event_id,
                visible_text: run.visible_text.clone(),
            }
        })
        .collect::<Vec<_>>();
    let status = options
        .status
        .clone()
        .unwrap_or_else(|| aggregate_session_status(&runs));
    let last_event_id = options.last_event_id.or_else(|| {
        runs.iter()
            .filter_map(|run| run.projection.last_event_id)
            .max()
    });
    let chain_root_hash = options.chain_root_hash.clone().or_else(|| {
        runs.iter()
            .rev()
            .find_map(|run| run.projection.prefix_hash.clone())
    });
    let mut view = SessionView {
        schema: SESSION_VIEW_SCHEMA.to_string(),
        schema_version: SESSION_VIEW_SCHEMA_VERSION,
        producer: ViewProducer::default(),
        session: SessionViewSession {
            session_id,
            parent_session_id: options.parent_session_id.clone().or_else(|| {
                runs.iter()
                    .find_map(|run| run.run.parent_session_id.clone())
            }),
            root_session_id: options.root_session_id.clone(),
            status,
            run_count: runs.len(),
            started_at,
            updated_at,
            last_event_id,
            chain_root_hash,
        },
        projection: ProjectionInfo {
            projection_id: String::new(),
            projection_hash: None,
            prefix_hash: None,
            last_event_id,
        },
        runs,
        history,
        usage,
        pending: dedupe_pending(pending),
        failure,
        metadata: SessionViewMetadata {
            record_count: 0,
            event_count: options.event_count,
            has_event_log: options.has_event_log,
        },
    };
    view.metadata.record_count = view.runs.len();
    view.projection.prefix_hash = view.session.chain_root_hash.clone();
    finalize_session_projection(&mut view);
    view
}

pub async fn build_session_view_from_run_records(
    runs: Vec<(&RunRecord, Option<String>)>,
    session_id: Option<String>,
    log: Option<&AnyEventLog>,
) -> Result<SessionView, RunViewError> {
    let mut views = Vec::new();
    for (run, path) in runs {
        views.push(build_run_view_with_event_log(run, path, log).await?);
    }
    let mut options = SessionViewOptions {
        session_id,
        has_event_log: log.is_some(),
        ..SessionViewOptions::default()
    };
    if let (Some(log), Some(session_id)) = (log, options.session_id.as_deref()) {
        let (last_event_id, chain_root_hash) = read_session_tip(log, session_id).await?;
        options.last_event_id = last_event_id;
        options.chain_root_hash = chain_root_hash;
    }
    Ok(build_session_view_from_run_views(views, options))
}

pub async fn build_empty_session_view(
    session_id: Option<String>,
    log: Option<&AnyEventLog>,
) -> Result<SessionView, RunViewError> {
    let mut options = SessionViewOptions {
        session_id: session_id.clone(),
        has_event_log: log.is_some(),
        ..SessionViewOptions::default()
    };
    if let (Some(log), Some(session_id)) = (log, session_id.as_deref()) {
        let (last_event_id, chain_root_hash) = read_session_tip(log, session_id).await?;
        options.last_event_id = last_event_id;
        options.chain_root_hash = chain_root_hash;
    }
    Ok(build_session_view_from_run_views(Vec::new(), options))
}

async fn read_session_tip(
    log: &AnyEventLog,
    session_id: &str,
) -> Result<(Option<EventId>, Option<String>), LogError> {
    let topic = crate::session_timeline::agent_events_topic(session_id);
    let Some(latest) = log.latest(&topic).await? else {
        return Ok((None, None));
    };
    let from = latest.checked_sub(1);
    let events = log.read_range(&topic, from, 1).await?;
    let prefix_hash = events
        .into_iter()
        .find(|(event_id, _)| *event_id == latest)
        .and_then(|(event_id, event)| {
            event_record_hash_from_headers(topic.as_str(), event_id, &event).ok()
        });
    Ok((Some(latest), prefix_hash))
}

fn build_child_view(child: &RunChildRecord, policy: &RedactionPolicy) -> RunViewChild {
    RunViewChild {
        worker_id: child.worker_id.clone(),
        worker_name: child.worker_name.clone(),
        run_id: child.run_id.clone(),
        session_id: child.session_id.clone(),
        parent_session_id: child.parent_session_id.clone(),
        run_path: child.run_path.clone(),
        status: child.status.clone(),
        task: redact_bounded(&child.task, policy, TEXT_LIMIT),
    }
}

fn build_stage_view(stage: &RunStageRecord, policy: &RedactionPolicy) -> RunViewStage {
    let usage = stage
        .usage
        .as_ref()
        .map(RunViewUsage::from)
        .unwrap_or_default();
    let artifact_refs = stage
        .produced_artifact_ids
        .iter()
        .chain(stage.artifacts.iter().map(|artifact| &artifact.id))
        .filter(|id| !id.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    RunViewStage {
        id: stage.id.clone(),
        node_id: stage.node_id.clone(),
        kind: stage.kind.clone(),
        status: stage.status.clone(),
        outcome: stage.outcome.clone(),
        branch: stage.branch.clone(),
        started_at: stage.started_at.clone(),
        finished_at: stage.finished_at.clone(),
        duration_ms: stage_duration_ms(stage),
        visible_text: stage
            .visible_text
            .as_deref()
            .map(|text| redact_bounded(text, policy, TEXT_LIMIT)),
        usage,
        provider: metadata_string_any(&stage.metadata, &["provider"])
            .or_else(|| metadata_path_string(&stage.metadata, &["model_policy", "provider"])),
        model: metadata_string_any(&stage.metadata, &["model"])
            .or_else(|| metadata_path_string(&stage.metadata, &["model_policy", "model"])),
        artifact_refs,
        attempt_count: stage.attempts.len(),
        error: stage_error(stage, policy),
    }
}

fn build_artifact_view(artifact: &ArtifactRecord, policy: &RedactionPolicy) -> RunViewArtifact {
    RunViewArtifact {
        id: artifact.id.clone(),
        kind: artifact.kind.clone(),
        title: artifact.title.clone(),
        source: artifact.source.clone(),
        stage: artifact.stage.clone(),
        estimated_tokens: artifact.estimated_tokens,
        lineage: artifact.lineage.clone(),
        preview: artifact
            .text
            .as_deref()
            .map(|text| redact_bounded(text, policy, PREVIEW_LIMIT))
            .or_else(|| {
                artifact
                    .data
                    .as_ref()
                    .map(|data| redact_json_preview(data, policy))
            }),
    }
}

fn build_checkpoint_view(checkpoint: &RunCheckpointRecord) -> RunViewCheckpoint {
    RunViewCheckpoint {
        id: checkpoint.id.clone(),
        reason: checkpoint.reason.clone(),
        ready_count: checkpoint.ready_nodes.len(),
        completed_count: checkpoint.completed_nodes.len(),
        last_stage_id: checkpoint.last_stage_id.clone(),
        persisted_at: checkpoint.persisted_at.clone(),
    }
}

fn build_approval_view(
    question: &RunHitlQuestionRecord,
    policy: &RedactionPolicy,
) -> RunViewApproval {
    RunViewApproval {
        request_id: question.request_id.clone(),
        prompt: redact_bounded(&question.prompt, policy, PREVIEW_LIMIT),
        agent: question.agent.clone(),
        trace_id: question.trace_id.clone(),
        asked_at: question.asked_at.clone(),
    }
}

fn provider_summary(run: &RunRecord) -> Vec<RunViewProvider> {
    let mut providers = BTreeMap::<(String, String), RunViewProvider>::new();
    for span in run
        .trace_spans
        .iter()
        .filter(|span| span.kind == "llm_call")
    {
        let provider = span
            .metadata
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let model = span
            .metadata
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let input_tokens = metadata_i64(&span.metadata, "input_tokens");
        let output_tokens = metadata_i64(&span.metadata, "output_tokens");
        let cost_usd = span
            .metadata
            .get("cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                crate::llm::calculate_cost_for_provider(
                    &provider,
                    &model,
                    input_tokens,
                    output_tokens,
                )
            });
        let entry = providers
            .entry((provider.clone(), model.clone()))
            .or_insert_with(|| RunViewProvider {
                provider,
                model,
                ..RunViewProvider::default()
            });
        entry.call_count += 1;
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
        entry.cost_usd += cost_usd;
    }
    if providers.is_empty() {
        if let Some(usage) = &run.usage {
            for model in &usage.models {
                if model.is_empty() {
                    continue;
                }
                providers.insert(
                    ("unknown".to_string(), model.clone()),
                    RunViewProvider {
                        provider: "unknown".to_string(),
                        model: model.clone(),
                        call_count: usage.call_count,
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cost_usd: usage.total_cost,
                    },
                );
            }
        }
    }
    providers.into_values().collect()
}

fn transcript_summary(value: Option<&Value>, policy: &RedactionPolicy) -> TranscriptSummary {
    let Some(value) = value else {
        return TranscriptSummary::default();
    };
    TranscriptSummary {
        present: true,
        message_count: count_array_field(value, "messages"),
        event_count: count_array_field(value, "events"),
        summary: value
            .get("summary")
            .and_then(Value::as_str)
            .map(|text| redact_bounded(text, policy, PREVIEW_LIMIT))
            .or_else(|| {
                value
                    .get("summary")
                    .map(|value| redact_json_preview(value, policy))
            }),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn pending_auth(run: &RunRecord, policy: &RedactionPolicy) -> Vec<RunViewAuth> {
    let mut auth = Vec::new();
    collect_auth_from_metadata(None, &run.metadata, &mut auth, policy);
    for stage in &run.stages {
        collect_auth_from_metadata(Some(&stage.id), &stage.metadata, &mut auth, policy);
    }
    auth
}

fn collect_auth_from_metadata(
    stage_id: Option<&str>,
    metadata: &BTreeMap<String, Value>,
    out: &mut Vec<RunViewAuth>,
    policy: &RedactionPolicy,
) {
    for key in ["pending_auth", "auth_required", "mcp_auth_required"] {
        let Some(value) = metadata.get(key) else {
            continue;
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    out.push(auth_from_value(stage_id, item, policy));
                }
            }
            Value::Object(_) => out.push(auth_from_value(stage_id, value, policy)),
            Value::Bool(true) => out.push(RunViewAuth {
                stage_id: stage_id.map(str::to_string),
                ..RunViewAuth::default()
            }),
            Value::String(message) => out.push(RunViewAuth {
                stage_id: stage_id.map(str::to_string),
                message: Some(redact_bounded(message, policy, PREVIEW_LIMIT)),
                ..RunViewAuth::default()
            }),
            _ => {}
        }
    }
}

fn auth_from_value(stage_id: Option<&str>, value: &Value, policy: &RedactionPolicy) -> RunViewAuth {
    let object = value.as_object();
    let field = |name: &str| {
        object
            .and_then(|object| object.get(name))
            .and_then(Value::as_str)
            .map(|text| redact_bounded(text, policy, PREVIEW_LIMIT))
    };
    RunViewAuth {
        provider: field("provider"),
        server: field("server").or_else(|| field("server_name")),
        scope: field("scope"),
        stage_id: stage_id.map(str::to_string).or_else(|| field("stage_id")),
        message: field("message").or_else(|| Some(redact_json_preview(value, policy))),
    }
}

fn failure_summary(run: &RunRecord, policy: &RedactionPolicy) -> Option<RunViewFailure> {
    run.stages
        .iter()
        .rev()
        .find(|stage| failed_status(&stage.status) || failed_status(&stage.outcome))
        .map(|stage| RunViewFailure {
            stage_id: Some(stage.id.clone()),
            node_id: Some(stage.node_id.clone()),
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            message: stage_error(stage, policy)
                .or_else(|| Some(format!("{} failed with {}", stage.node_id, stage.outcome))),
        })
        .or_else(|| {
            failed_status(&run.status).then(|| RunViewFailure {
                status: run.status.clone(),
                outcome: run.status.clone(),
                ..RunViewFailure::default()
            })
        })
}

fn stage_error(stage: &RunStageRecord, policy: &RedactionPolicy) -> Option<String> {
    stage
        .metadata
        .get("error")
        .map(|value| redact_json_preview(value, policy))
        .or_else(|| {
            stage
                .attempts
                .iter()
                .rev()
                .find_map(|attempt| attempt.error.as_deref())
                .map(|error| redact_bounded(error, policy, PREVIEW_LIMIT))
        })
}

fn failed_status(value: &str) -> bool {
    matches!(
        value,
        "failed" | "error" | "errored" | "cancelled" | "canceled" | "timeout" | "timed_out"
    )
}

fn usage_from_stages(stages: &[RunViewStage]) -> RunViewUsage {
    let mut usage = RunViewUsage::default();
    for stage in stages {
        usage.add_usage(&stage.usage);
    }
    usage
}

fn run_duration_ms(run: &RunRecord) -> Option<u64> {
    let from_usage = run
        .usage
        .as_ref()
        .and_then(|usage| u64::try_from(usage.total_duration_ms).ok())
        .filter(|duration| *duration > 0);
    let from_spans = run
        .trace_spans
        .iter()
        .map(trace_span_end_ms)
        .max()
        .filter(|duration| *duration > 0);
    let from_timestamps = run
        .finished_at
        .as_deref()
        .and_then(|finished| timestamp_delta_ms(&run.started_at, finished));
    from_timestamps.or(from_spans).or(from_usage)
}

fn stage_duration_ms(stage: &RunStageRecord) -> Option<u64> {
    stage
        .usage
        .as_ref()
        .and_then(|usage| u64::try_from(usage.total_duration_ms).ok())
        .filter(|duration| *duration > 0)
        .or_else(|| {
            stage
                .finished_at
                .as_deref()
                .and_then(|finished| timestamp_delta_ms(&stage.started_at, finished))
        })
}

fn timestamp_delta_ms(started_at: &str, finished_at: &str) -> Option<u64> {
    let start = parse_timestamp_ms(started_at)?;
    let end = parse_timestamp_ms(finished_at)?;
    u64::try_from(end.saturating_sub(start)).ok()
}

fn parse_timestamp_ms(value: &str) -> Option<i128> {
    if value.trim().is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<i128>() {
        return Some(seconds.saturating_mul(1000));
    }
    let parsed = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    Some(
        i128::from(parsed.unix_timestamp()).saturating_mul(1000) + i128::from(parsed.millisecond()),
    )
}

fn trace_span_end_ms(span: &RunTraceSpanRecord) -> u64 {
    span.start_ms.saturating_add(span.duration_ms)
}

fn infer_run_session_id(run: &RunRecord) -> Option<String> {
    metadata_string_any(&run.metadata, &["session_id", "agent_session_id"])
        .or_else(|| metadata_path_string(&run.metadata, &["model_policy", "session_id"]))
        .or_else(|| metadata_path_string(&run.metadata, &["audit", "session_id"]))
        .or_else(|| {
            run.child_runs
                .iter()
                .find_map(|child| child.session_id.clone())
        })
        .or_else(|| {
            run.stages.iter().find_map(|stage| {
                metadata_string_any(&stage.metadata, &["session_id", "agent_session_id"])
                    .or_else(|| {
                        metadata_path_string(&stage.metadata, &["model_policy", "session_id"])
                    })
                    .or_else(|| metadata_path_string(&stage.metadata, &["audit", "session_id"]))
                    .or_else(|| {
                        metadata_path_string(&stage.metadata, &["worker", "audit", "session_id"])
                    })
            })
        })
        .or_else(|| {
            run.trace_spans.iter().find_map(|span| {
                metadata_string_any(&span.metadata, &["session_id", "agent_session_id"])
            })
        })
}

fn infer_parent_session_id(run: &RunRecord) -> Option<String> {
    metadata_string_any(&run.metadata, &["parent_session_id"])
        .or_else(|| metadata_path_string(&run.metadata, &["audit", "parent_session_id"]))
        .or_else(|| {
            run.child_runs
                .iter()
                .find_map(|child| child.parent_session_id.clone())
        })
        .or_else(|| {
            run.stages.iter().find_map(|stage| {
                metadata_string_any(&stage.metadata, &["parent_session_id"])
                    .or_else(|| {
                        metadata_path_string(&stage.metadata, &["audit", "parent_session_id"])
                    })
                    .or_else(|| {
                        metadata_path_string(
                            &stage.metadata,
                            &["worker", "audit", "parent_session_id"],
                        )
                    })
            })
        })
}

fn metadata_string_any(metadata: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metadata_path_string(metadata: &BTreeMap<String, Value>, path: &[&str]) -> Option<String> {
    let mut value = metadata.get(*path.first()?)?;
    for key in &path[1..] {
        value = value.get(*key)?;
    }
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metadata_i64(metadata: &BTreeMap<String, Value>, key: &str) -> i64 {
    metadata
        .get(key)
        .and_then(Value::as_i64)
        .or_else(|| {
            metadata
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|value| i64::try_from(value).ok())
        })
        .unwrap_or_default()
}

fn count_array_field(value: &Value, field: &str) -> usize {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

fn redact_json_preview(value: &Value, policy: &RedactionPolicy) -> String {
    let mut value = value.clone();
    policy.redact_json_in_place(&mut value);
    bounded_text(
        &serde_json::to_string(&value).unwrap_or_default(),
        PREVIEW_LIMIT,
    )
}

fn redact_bounded(text: &str, policy: &RedactionPolicy, limit: usize) -> String {
    let redacted = policy.redact_string(text);
    bounded_text(redacted.as_ref(), limit)
}

fn bounded_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let boundary = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit)
        .last()
        .unwrap_or(0);
    format!("{}...", &text[..boundary])
}

fn bounded_join(values: impl IntoIterator<Item = String>, limit: usize) -> Option<String> {
    let mut out = String::new();
    for value in values {
        if value.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&value);
        if out.len() > limit {
            return Some(bounded_text(&out, limit));
        }
    }
    non_empty_string(&out)
}

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn min_opt_string(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn max_opt_string(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn aggregate_session_status(runs: &[RunView]) -> String {
    if runs.is_empty() {
        return "unknown".to_string();
    }
    if runs
        .iter()
        .any(|run| failed_status(&run.run.status) || run.failure.is_some())
    {
        return "failed".to_string();
    }
    if runs.iter().all(|run| {
        matches!(
            run.run.status.as_str(),
            "completed" | "succeeded" | "success" | "ok"
        )
    }) {
        return "completed".to_string();
    }
    "active".to_string()
}

fn dedupe_pending(mut pending: RunViewPendingState) -> RunViewPendingState {
    let mut nodes = BTreeSet::new();
    pending.nodes.retain(|node| nodes.insert(node.clone()));
    let mut approvals = BTreeSet::new();
    pending
        .approvals
        .retain(|approval| approvals.insert(approval.request_id.clone()));
    let mut auth_seen = BTreeSet::new();
    pending.auth.retain(|item| {
        auth_seen.insert((
            item.provider.clone(),
            item.server.clone(),
            item.scope.clone(),
            item.stage_id.clone(),
        ))
    });
    pending
}

fn finalize_run_projection(view: &mut RunView) {
    if let Some(hash) = projection_hash(RUN_VIEW_SCHEMA, view) {
        view.projection.projection_id =
            format!("run_view:{}:{}", view.run.run_id, hash_suffix(&hash));
        view.projection.projection_hash = Some(hash);
    } else {
        view.projection.projection_id = format!("run_view:{}", view.run.run_id);
    }
}

fn finalize_session_projection(view: &mut SessionView) {
    let id = view
        .session
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(hash) = projection_hash(SESSION_VIEW_SCHEMA, view) {
        view.projection.projection_id = format!("session_view:{id}:{}", hash_suffix(&hash));
        view.projection.projection_hash = Some(hash);
    } else {
        view.projection.projection_id = format!("session_view:{id}");
    }
}

fn projection_hash<T: Serialize>(schema: &str, value: &T) -> Option<String> {
    let mut value = serde_json::to_value(value).ok()?;
    if let Some(projection) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("projection"))
        .and_then(Value::as_object_mut)
    {
        projection.remove("projection_id");
        projection.remove("projection_hash");
    }
    let bytes = serde_json::to_vec(&value).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(schema.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    Some(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn hash_suffix(hash: &str) -> String {
    hash.strip_prefix("sha256:")
        .unwrap_or(hash)
        .chars()
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_run() -> RunRecord {
        RunRecord {
            type_name: "run_record".to_string(),
            id: "run_1".to_string(),
            workflow_id: "wf".to_string(),
            workflow_name: Some("Workflow".to_string()),
            task: "do work".to_string(),
            status: "completed".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: Some("2026-01-01T00:00:02Z".to_string()),
            stages: vec![RunStageRecord {
                id: "stage_1".to_string(),
                node_id: "plan".to_string(),
                kind: "llm".to_string(),
                status: "completed".to_string(),
                outcome: "ok".to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
                finished_at: Some("2026-01-01T00:00:01Z".to_string()),
                visible_text: Some("done".to_string()),
                usage: Some(LlmUsageRecord {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_duration_ms: 1000,
                    call_count: 1,
                    total_cost: 0.01,
                    models: vec!["model-a".to_string()],
                }),
                metadata: BTreeMap::from([
                    ("session_id".to_string(), json!("session_1")),
                    ("provider".to_string(), json!("test")),
                    ("model".to_string(), json!("model-a")),
                ]),
                ..RunStageRecord::default()
            }],
            trace_spans: vec![RunTraceSpanRecord {
                kind: "llm_call".to_string(),
                metadata: BTreeMap::from([
                    ("provider".to_string(), json!("test")),
                    ("model".to_string(), json!("model-a")),
                    ("input_tokens".to_string(), json!(10)),
                    ("output_tokens".to_string(), json!(5)),
                    ("cost_usd".to_string(), json!(0.01)),
                ]),
                ..RunTraceSpanRecord::default()
            }],
            transcript: Some(json!({
                "source": "inline",
                "summary": "short",
                "messages": [{"role": "assistant"}],
                "events": [{"kind": "output"}]
            })),
            ..RunRecord::default()
        }
    }

    #[test]
    fn build_run_view_projects_stable_public_fields() {
        let view = build_run_view_with_path(&sample_run(), Some("runs/run_1.json"));
        assert_eq!(view.schema, RUN_VIEW_SCHEMA);
        assert_eq!(view.schema_version, RUN_VIEW_SCHEMA_VERSION);
        assert_eq!(view.run.run_id, "run_1");
        assert_eq!(view.run.session_id.as_deref(), Some("session_1"));
        assert_eq!(view.run.run_path.as_deref(), Some("runs/run_1.json"));
        assert_eq!(view.run.duration_ms, Some(2000));
        assert_eq!(view.visible_text.as_deref(), Some("done"));
        assert_eq!(view.transcript.message_count, 1);
        assert_eq!(view.usage.input_tokens, 10);
        assert_eq!(view.providers.len(), 1);
        assert!(view.projection.projection_id.starts_with("run_view:run_1:"));
        assert!(view.projection.projection_hash.is_some());
    }

    #[test]
    fn build_run_view_tolerates_sparse_legacy_records() {
        let run = RunRecord {
            type_name: "run_record".to_string(),
            id: "legacy".to_string(),
            status: "failed".to_string(),
            ..RunRecord::default()
        };
        let view = build_run_view(&run);
        assert_eq!(view.run.run_id, "legacy");
        assert_eq!(view.run.session_id, None);
        assert_eq!(view.transcript.present, false);
        assert_eq!(
            view.failure.as_ref().map(|failure| failure.status.as_str()),
            Some("failed")
        );
    }

    #[test]
    fn build_session_view_aggregates_runs() {
        let run = build_run_view(&sample_run());
        let view = build_session_view_from_run_views(
            vec![run],
            SessionViewOptions {
                session_id: Some("session_1".to_string()),
                last_event_id: Some(7),
                chain_root_hash: Some("sha256:abc".to_string()),
                ..SessionViewOptions::default()
            },
        );
        assert_eq!(view.schema, SESSION_VIEW_SCHEMA);
        assert_eq!(view.session.session_id.as_deref(), Some("session_1"));
        assert_eq!(view.session.last_event_id, Some(7));
        assert_eq!(view.session.chain_root_hash.as_deref(), Some("sha256:abc"));
        assert_eq!(view.history.len(), 1);
        assert_eq!(view.usage.call_count, 1);
        assert!(view
            .projection
            .projection_id
            .starts_with("session_view:session_1:"));
    }

    #[test]
    fn build_run_view_redacts_child_tasks_and_approvals() {
        let mut run = sample_run();
        run.child_runs.push(RunChildRecord {
            worker_id: "worker_1".to_string(),
            worker_name: "worker".to_string(),
            task: "inspect AKIAABCDEFGHIJKLMNOP".to_string(),
            ..RunChildRecord::default()
        });
        run.hitl_questions.push(RunHitlQuestionRecord {
            request_id: "approval_1".to_string(),
            prompt: "approve AKIAABCDEFGHIJKLMNOP".to_string(),
            ..RunHitlQuestionRecord::default()
        });

        let view = build_run_view(&run);
        assert!(!view.run.child_runs[0].task.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(!view.pending.approvals[0]
            .prompt
            .contains("AKIAABCDEFGHIJKLMNOP"));
    }
}
