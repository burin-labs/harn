//! One typed, redacted report over a persisted run tree and its event timeline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::event_log::{AnyEventLog, SqliteEventLog};
use crate::redact::current_policy;
use crate::session_timeline::{
    query_session_timeline, SessionTimelineQuery, SessionTimelineSnapshot,
};

use super::persistence::load_run_record_snapshot;
use super::time::parse_timestamp_ms;
use super::{build_run_view_with_event_log, RunRecord, RunView, RunViewUsage, ViewProducer};

mod join_evidence;
#[cfg(test)]
mod join_evidence_tests;

use join_evidence::{project_join_evidence, JoinEvidenceProjection};

pub const RUN_REPORT_SCHEMA: &str = "harn.run_report.v1";
pub const RUN_REPORT_SCHEMA_VERSION: u32 = 1;
const MAX_RUN_TREE_DEPTH: usize = 64;
const MAX_RUN_TREE_NODES: usize = 1024;
const MAX_RUN_TREE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct RunReportRequest {
    pub run_record_path: PathBuf,
    pub events_db: Option<PathBuf>,
    /// Empty for a trusted local CLI call. Adapters that accept remote paths
    /// must provide every root from which the report may read.
    pub allowed_roots: Vec<PathBuf>,
    pub source_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunReport {
    pub schema: String,
    pub schema_version: u32,
    pub producer: ViewProducer,
    pub projection: RunReportProjection,
    pub root_run_id: String,
    pub agents: Vec<RunReportAgent>,
    pub delegations: Vec<RunReportDelegation>,
    pub llm_calls: Vec<RunReportLlmCall>,
    pub coordination: RunReportCoordination,
    pub timelines: Vec<SessionTimelineSnapshot>,
    pub sources: Vec<RunReportSource>,
    pub checks: Vec<RunReportCheck>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportProjection {
    pub id: String,
    pub hash: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunReportAgent {
    pub agent_id: String,
    pub worker_id: Option<String>,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_agent_id: Option<String>,
    pub status: String,
    pub task: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub usage: RunViewUsage,
    pub visible_output: Option<String>,
    pub execution: Option<RunReportExecution>,
    pub capability_policy: Option<super::super::CapabilityPolicy>,
    pub mutation_scope: Option<String>,
    pub approval_policy: Option<super::super::ToolApprovalPolicy>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportExecution {
    pub cwd: Option<String>,
    pub project_root: Option<String>,
    pub repo_path: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub environment_policy: crate::security::EnvironmentPolicyKind,
    pub grants: Vec<crate::security::GrantReceipt>,
}

impl From<&super::RunExecutionRecord> for RunReportExecution {
    fn from(execution: &super::RunExecutionRecord) -> Self {
        Self {
            cwd: execution.cwd.clone(),
            project_root: execution.project_root.clone(),
            repo_path: execution.repo_path.clone(),
            worktree_path: execution.worktree_path.clone(),
            branch: execution.branch.clone(),
            environment_policy: execution.environment_policy,
            grants: execution.grants.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportDelegation {
    pub parent_agent_id: String,
    pub child_agent_id: String,
    pub worker_id: String,
    pub status: String,
    pub parent_observed_status: String,
    pub child_observed_status: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub forward_pointer: bool,
    pub back_pointer: bool,
    pub session_pointer_consistent: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunReportLlmCall {
    pub agent_id: String,
    pub call_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub ttft_ms: Option<u64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportCoordination {
    pub spawned: usize,
    pub terminal: usize,
    pub open: usize,
    pub orphaned: usize,
    /// Terminal children without a canonical join receipt. `None` when the
    /// selected event evidence is missing, malformed, or truncated.
    pub unjoined: Option<usize>,
    pub max_concurrent_children: Option<usize>,
    /// Maximum observed parent wait, from the moment the parent began waiting
    /// to the moment it collected the child (#6074). `None` when join evidence
    /// is incomplete, or when no child was ever waited on — which is a real
    /// state, not a gap, and is why this is not zero.
    pub observed_wait_ms: Option<u64>,
    /// Maximum observed child-terminal-to-parent-collection lag. `None` when
    /// join evidence is incomplete or no valid lag was observed.
    pub observed_join_ms: Option<u64>,
    /// Maximum observed result-collapsing duration. `None` when join evidence
    /// is incomplete, or when collection happened without collapsing a result.
    ///
    /// Kept apart from `observed_wait_ms` and `observed_join_ms` because these
    /// are three different costs — scheduler wait, collection lag, and the
    /// parent's own work — and one number covering all three cannot say which
    /// one a slow run is paying.
    pub observed_result_processing_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportSource {
    pub id: String,
    pub agent_id: Option<String>,
    pub kind: String,
    pub path: Option<String>,
    pub sha256: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunReportCheck {
    pub code: String,
    pub severity: String,
    pub status: String,
    pub agent_id: Option<String>,
    pub message: String,
}

#[derive(Debug)]
pub enum RunReportError {
    Read(String),
    EventLog(String),
    Encode(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunReportValidationError {
    Schema(String),
    Hash(String),
    Encode(String),
}

#[derive(Debug)]
struct LoadedRunTree {
    records: Vec<RunRecord>,
    paths: Vec<PathBuf>,
    hashes: Vec<String>,
}

impl std::fmt::Display for RunReportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(message) | Self::EventLog(message) | Self::Encode(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for RunReportError {}

impl std::fmt::Display for RunReportValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(message) | Self::Hash(message) | Self::Encode(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for RunReportValidationError {}

/// Validate that a report is the supported typed projection and that its
/// content still matches the producer-owned projection hash.
pub fn validate_run_report(report: &RunReport) -> Result<(), RunReportValidationError> {
    if report.schema != RUN_REPORT_SCHEMA || report.schema_version != RUN_REPORT_SCHEMA_VERSION {
        return Err(RunReportValidationError::Schema(format!(
            "expected {RUN_REPORT_SCHEMA} schema version {RUN_REPORT_SCHEMA_VERSION}, got {:?} version {}",
            report.schema, report.schema_version
        )));
    }
    let expected = run_report_projection_hash(report)?;
    if report.projection.hash != expected {
        return Err(RunReportValidationError::Hash(format!(
            "run report projection hash mismatch: expected {expected}, got {:?}",
            report.projection.hash
        )));
    }
    Ok(())
}

/// Recompute the logical report hash using the same canonical contract as the
/// report producer. The hash field itself is cleared before canonicalization.
pub fn run_report_projection_hash(report: &RunReport) -> Result<String, RunReportValidationError> {
    let value = serde_json::to_value(report)
        .map_err(|error| RunReportValidationError::Encode(error.to_string()))?;
    Ok(run_report_projection_hash_value(&value))
}

fn run_report_projection_hash_value(value: &Value) -> String {
    let mut value = value.clone();
    value["projection"]["hash"] = Value::String(String::new());
    let digest = Sha256::digest(crate::canonical_json::to_vec(&value));
    format!("sha256:{}", hex::encode(digest))
}

pub async fn build_run_report(request: RunReportRequest) -> Result<RunReport, RunReportError> {
    let allowed_roots = canonical_allowed_roots(&request.allowed_roots)?;
    let root_path = checked_path(&request.run_record_path, &allowed_roots)?;
    let source_root = request
        .source_root
        .as_deref()
        .and_then(|path| path.canonicalize().ok());
    let events_db_path = request
        .events_db
        .as_deref()
        .map(|path| checked_path(path, &allowed_roots))
        .transpose()?;
    let event_log = match events_db_path.as_deref() {
        Some(path) => Some(AnyEventLog::Sqlite(
            SqliteEventLog::open_read_only(path.to_path_buf(), 16)
                .map_err(|error| RunReportError::EventLog(error.to_string()))?,
        )),
        None => None,
    };

    let tree = tokio::task::spawn_blocking(move || load_run_tree(&root_path, &allowed_roots))
        .await
        .map_err(|error| RunReportError::Read(format!("load run tree task: {error}")))??;
    let root_run_id = tree
        .records
        .first()
        .map(|record| record.id.clone())
        .unwrap_or_default();

    let mut views = Vec::with_capacity(tree.records.len());
    let mut timelines = Vec::new();
    for (record, path) in tree.records.iter().zip(&tree.paths) {
        let view = build_run_view_with_event_log(record, None::<String>, event_log.as_ref())
            .await
            .map_err(|error| RunReportError::EventLog(error.to_string()))?;
        let query = SessionTimelineQuery {
            session_id: view.run.session_id.clone(),
            run_id: Some(record.id.clone()),
            run_path: Some(path.to_string_lossy().into_owned()),
            ..SessionTimelineQuery::default()
        };
        let timeline = query_session_timeline(event_log.as_ref(), Some(record), query)
            .await
            .map_err(|error| RunReportError::EventLog(error.to_string()))?;
        if event_log.is_some() || !timeline.nodes.is_empty() {
            timelines.push(timeline);
        }
        views.push(view);
    }

    let report = assemble_report(
        root_run_id,
        &tree.records,
        &tree.paths,
        &tree.hashes,
        &views,
        timelines,
        source_root.as_deref(),
        events_db_path.as_deref(),
    )?;
    let mut value =
        serde_json::to_value(&report).map_err(|error| RunReportError::Encode(error.to_string()))?;
    value["projection"]["hash"] = Value::String(String::new());
    current_policy().redact_json_in_place(&mut value);
    value["projection"]["hash"] = Value::String(run_report_projection_hash_value(&value));
    serde_json::from_value(value).map_err(|error| RunReportError::Encode(error.to_string()))
}

fn assemble_report(
    root_run_id: String,
    records: &[RunRecord],
    record_paths: &[PathBuf],
    record_hashes: &[String],
    views: &[RunView],
    timelines: Vec<SessionTimelineSnapshot>,
    source_root: Option<&Path>,
    events_db_path: Option<&Path>,
) -> Result<RunReport, RunReportError> {
    let views_by_id: BTreeMap<_, _> = views
        .iter()
        .map(|view| (view.run.run_id.as_str(), view))
        .collect();
    let records_by_id: BTreeMap<_, _> = records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    if records_by_id.len() != records.len() {
        return Err(RunReportError::Read(
            "run tree contains duplicate run ids; lineage would be ambiguous".to_string(),
        ));
    }
    let records_by_path: BTreeMap<_, _> = record_paths
        .iter()
        .zip(records)
        .map(|(path, record)| (path.clone(), record))
        .collect();
    let mut checks = Vec::new();
    let mut agents = Vec::new();
    let mut delegations = Vec::new();
    let mut llm_calls = Vec::new();
    let mut sources = Vec::new();
    let mut forward_edges = BTreeSet::new();

    for (((record, path), hash), view) in records
        .iter()
        .zip(record_paths)
        .zip(record_hashes)
        .zip(views)
    {
        agents.push(agent_from_view(view, None, record.policy.clone()));
        sources.push(source_for_snapshot(
            format!("run:{}", record.id),
            "run_record",
            path,
            hash,
            source_root,
            Some(format!("run:{}", record.id)),
        ));
        if let Some(observability) = &record.observability {
            for pointer in &observability.transcript_pointers {
                sources.push(RunReportSource {
                    id: format!("run:{}:{}", record.id, pointer.id),
                    agent_id: Some(format!("run:{}", record.id)),
                    kind: pointer.kind.clone(),
                    path: pointer
                        .path
                        .as_deref()
                        .map(|path| display_path(Path::new(path), source_root)),
                    sha256: pointer
                        .descriptor
                        .as_ref()
                        .map(|descriptor| descriptor.sha256.clone()),
                    status: pointer.verification_status.clone(),
                    error: pointer.verification_error.clone(),
                });
            }
        }
        for span in &record.trace_spans {
            if span.kind != "llm_call" {
                continue;
            }
            llm_calls.push(llm_call_from_span(&record.id, span));
        }
        for child in &record.child_runs {
            let path_record = child
                .run_path
                .as_deref()
                .map(PathBuf::from)
                .map(|child_path| resolve_child_path(path, child_path))
                .and_then(|child_path| child_path.canonicalize().ok())
                .and_then(|child_path| records_by_path.get(&child_path).copied());
            let id_record = child
                .run_id
                .as_deref()
                .and_then(|id| records_by_id.get(id).copied());
            let child_record = path_record.or(id_record);
            let child_agent_id = child_record
                .map(|record| format!("run:{}", record.id))
                .or_else(|| child.run_id.as_ref().map(|run_id| format!("run:{run_id}")))
                .unwrap_or_else(|| format!("worker:{}", child.worker_id));
            let child_view =
                child_record.and_then(|record| views_by_id.get(record.id.as_str()).copied());
            let back_pointer = child_record
                .and_then(|child_record| child_record.parent_run_id.as_deref())
                == Some(record.id.as_str());
            let session_pointer_consistent = session_pointers_consistent(view, child, child_view);
            let child_observed_status = child_record.map(|record| record.status.clone());
            let status = child_observed_status
                .as_deref()
                .filter(|status| !status.is_empty())
                .unwrap_or(&child.status)
                .to_string();
            delegations.push(RunReportDelegation {
                parent_agent_id: format!("run:{}", record.id),
                child_agent_id: child_agent_id.clone(),
                worker_id: child.worker_id.clone(),
                status,
                parent_observed_status: child.status.clone(),
                child_observed_status: child_observed_status.clone(),
                started_at: child_record
                    .and_then(|record| nonempty(&record.started_at))
                    .or_else(|| nonempty(&child.started_at)),
                finished_at: child_record
                    .and_then(|record| record.finished_at.clone())
                    .or_else(|| child.finished_at.clone()),
                forward_pointer: true,
                back_pointer,
                session_pointer_consistent,
            });
            if let Some(child_record) = child_record {
                forward_edges.insert((record.id.clone(), child_record.id.clone()));
                if child.run_id.is_none() {
                    checks.push(check(
                        "child_run_id_missing",
                        "warning",
                        &child_agent_id,
                        format!(
                            "worker {} was correlated by run_path because its forward run_id is missing",
                            child.worker_id
                        ),
                    ));
                } else if child.run_id.as_deref() != Some(child_record.id.as_str()) {
                    checks.push(check(
                        "child_run_id_mismatch",
                        "error",
                        &child_agent_id,
                        format!(
                            "worker {} points to run id {:?}, but run_path contains {}",
                            child.worker_id, child.run_id, child_record.id
                        ),
                    ));
                }
                if let Some(agent) = agents
                    .iter_mut()
                    .find(|agent| agent.agent_id == child_agent_id)
                {
                    agent.worker_id = Some(child.worker_id.clone());
                    agent.mutation_scope = child.mutation_scope.clone();
                    agent.approval_policy = child.approval_policy.clone();
                }
            }
            if child_record.is_none() {
                agents.push(agent_from_child(child, &record.id));
                checks.push(check(
                    "child_run_missing",
                    "error",
                    &child_agent_id,
                    format!(
                        "worker {} has no readable child run record",
                        child.worker_id
                    ),
                ));
            } else if !back_pointer {
                checks.push(check(
                    "child_back_pointer_mismatch",
                    "error",
                    &child_agent_id,
                    format!("child does not point back to parent run {}", record.id),
                ));
            }
            if child_observed_status
                .as_deref()
                .is_some_and(|status| !child.status.is_empty() && status != child.status)
            {
                checks.push(check(
                    "child_status_mismatch",
                    "warning",
                    &child_agent_id,
                    format!(
                        "parent recorded status {:?}, while the child run recorded {:?}",
                        child.status, child_observed_status
                    ),
                ));
            }
            if session_pointer_consistent == Some(false) {
                checks.push(check(
                    "child_session_pointer_mismatch",
                    "error",
                    &child_agent_id,
                    "child and parent disagree about parent session".to_string(),
                ));
            }
        }
    }

    if let Some(path) = events_db_path {
        sources.push(RunReportSource {
            id: "events:sqlite".to_string(),
            agent_id: None,
            kind: "event_log".to_string(),
            path: Some(display_path(path, source_root)),
            sha256: None,
            status: "queried_read_only".to_string(),
            error: Some(
                "live SQLite evidence is cursor-scoped; no unstable whole-file hash was claimed"
                    .to_string(),
            ),
        });
    }

    for record in records {
        if let Some(parent_id) = record.parent_run_id.as_deref() {
            if !records_by_id.contains_key(parent_id) {
                checks.push(check(
                    "parent_run_missing",
                    "error",
                    &format!("run:{}", record.id),
                    format!("parent run {parent_id} is not present in the report"),
                ));
            } else if !forward_edges.contains(&(parent_id.to_string(), record.id.clone())) {
                delegations.push(RunReportDelegation {
                    parent_agent_id: format!("run:{parent_id}"),
                    child_agent_id: format!("run:{}", record.id),
                    back_pointer: true,
                    ..RunReportDelegation::default()
                });
                checks.push(check(
                    "parent_forward_pointer_missing",
                    "error",
                    &format!("run:{}", record.id),
                    format!("parent run {parent_id} does not list this child"),
                ));
            }
        }
    }

    for timeline in &timelines {
        if !timeline.coverage.truncated {
            continue;
        }
        let agent_id = timeline
            .query
            .run_id
            .as_deref()
            .map(|run_id| format!("run:{run_id}"))
            .unwrap_or_else(|| format!("run:{root_run_id}"));
        let availability = timeline
            .coverage
            .available
            .map(|available| format!(" of {available} available"))
            .unwrap_or_else(|| ", with the total available count unknown".to_string());
        checks.push(check(
            "timeline_truncated",
            "warning",
            &agent_id,
            format!(
                "timeline returned {}{}; later evidence may be omitted, so absence must not be inferred",
                timeline.coverage.returned, availability
            ),
        ));
    }

    let join_evidence = project_join_evidence(&delegations, &timelines, events_db_path.is_some());
    checks.extend(join_evidence.checks.iter().cloned());
    let coordination = coordination_summary(&delegations, &checks, &join_evidence);
    if !delegations.is_empty() {
        if coordination.max_concurrent_children.is_none() {
            checks.push(RunReportCheck {
                code: "coordination_intervals_incomplete".to_string(),
                severity: "info".to_string(),
                status: "unavailable".to_string(),
                agent_id: Some(format!("run:{root_run_id}")),
                message: "one or more child intervals lack a parseable start or finish timestamp, so peak concurrency is unknown".to_string(),
            });
        }
        // Only claim an interval is unavailable when it actually is. Since
        // #6074 the receipts carry wait and result-processing boundaries, so
        // this check names the ones still missing rather than asserting a
        // blanket gap that has been closed.
        let unavailable: Vec<&str> = [
            ("parent wait", coordination.observed_wait_ms.is_none()),
            (
                "terminal-to-collection lag",
                coordination.observed_join_ms.is_none(),
            ),
            (
                "result-processing time",
                coordination.observed_result_processing_ms.is_none(),
            ),
        ]
        .into_iter()
        .filter_map(|(label, missing)| missing.then_some(label))
        .collect();
        if !unavailable.is_empty() {
            checks.push(RunReportCheck {
                code: "coordination_timing_unavailable".to_string(),
                severity: "info".to_string(),
                status: "unavailable".to_string(),
                agent_id: Some(format!("run:{root_run_id}")),
                message: if join_evidence.complete {
                    format!(
                        "no canonical boundary was observed for {}, so {} remain{} unknown",
                        unavailable.join(", "),
                        if unavailable.len() == 1 { "it" } else { "they" },
                        if unavailable.len() == 1 { "s" } else { "" },
                    )
                } else {
                    "canonical join evidence is missing, malformed, or truncated, so unjoined children, parent wait, terminal-to-collection lag, and result-processing time remain unknown".to_string()
                },
            });
        }
    }

    agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    agents.dedup_by(|left, right| left.agent_id == right.agent_id);
    delegations.sort_by(|left, right| {
        (&left.parent_agent_id, &left.child_agent_id, &left.worker_id).cmp(&(
            &right.parent_agent_id,
            &right.child_agent_id,
            &right.worker_id,
        ))
    });
    llm_calls.sort_by_key(|call| (call.start_ms, call.call_id.clone()));
    sources.sort_by(|left, right| left.id.cmp(&right.id));
    checks.sort_by(|left, right| (&left.code, &left.agent_id).cmp(&(&right.code, &right.agent_id)));

    Ok(RunReport {
        schema: RUN_REPORT_SCHEMA.to_string(),
        schema_version: RUN_REPORT_SCHEMA_VERSION,
        producer: ViewProducer::default(),
        projection: RunReportProjection {
            id: format!("run_report:{root_run_id}"),
            hash: String::new(),
        },
        root_run_id,
        agents,
        delegations,
        llm_calls,
        coordination,
        timelines,
        sources,
        checks,
    })
}

fn load_run_tree(path: &Path, allowed_roots: &[PathBuf]) -> Result<LoadedRunTree, RunReportError> {
    let mut tree = LoadedRunTree {
        records: Vec::new(),
        paths: Vec::new(),
        hashes: Vec::new(),
    };
    let mut seen_paths = BTreeSet::new();
    let mut pending = vec![(path.to_path_buf(), 0_usize)];
    let mut total_bytes = 0_usize;

    while let Some((candidate, depth)) = pending.pop() {
        if depth > MAX_RUN_TREE_DEPTH {
            return Err(RunReportError::Read(format!(
                "run tree exceeds maximum depth {MAX_RUN_TREE_DEPTH}"
            )));
        }
        let path = checked_path(&candidate, allowed_roots)?;
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        if seen_paths.len() > MAX_RUN_TREE_NODES {
            return Err(RunReportError::Read(format!(
                "run tree exceeds maximum node count {MAX_RUN_TREE_NODES}"
            )));
        }
        let (record, bytes) = load_run_record_snapshot(&path).map_err(|error| {
            RunReportError::Read(format!("load run record {}: {error}", path.display()))
        })?;
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_RUN_TREE_BYTES {
            return Err(RunReportError::Read(format!(
                "run tree exceeds maximum persisted size {MAX_RUN_TREE_BYTES} bytes"
            )));
        }
        let mut child_paths = record
            .child_runs
            .iter()
            .filter_map(|child| child.run_path.as_deref())
            .map(PathBuf::from)
            .map(|child| resolve_child_path(&path, child))
            .filter(|child| child.exists())
            .map(|child| (child, depth + 1))
            .collect::<Vec<_>>();
        child_paths.reverse();
        pending.extend(child_paths);
        tree.hashes
            .push(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))));
        tree.records.push(record);
        tree.paths.push(path);
    }
    Ok(tree)
}

fn resolve_child_path(parent_path: &Path, child: PathBuf) -> PathBuf {
    if child.is_absolute() {
        child
    } else {
        parent_path.parent().unwrap_or(Path::new(".")).join(child)
    }
}

fn canonical_allowed_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, RunReportError> {
    roots
        .iter()
        .map(|root| {
            root.canonicalize().map_err(|error| {
                RunReportError::Read(format!("resolve allowed root {}: {error}", root.display()))
            })
        })
        .collect()
}

fn checked_path(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, RunReportError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| RunReportError::Read(format!("resolve {}: {error}", path.display())))?;
    if !allowed_roots.is_empty() && !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(RunReportError::Read(format!(
            "path {} is outside the report's allowed roots",
            path.display()
        )));
    }
    Ok(canonical)
}

pub(crate) fn read_checked_run_report_bytes(
    path: &Path,
    allowed_roots: &[PathBuf],
) -> Result<Vec<u8>, RunReportError> {
    let allowed_roots = canonical_allowed_roots(allowed_roots)?;
    let path = checked_path(path, &allowed_roots)?;
    std::fs::read(&path).map_err(|error| {
        RunReportError::Read(format!("read run report {}: {error}", path.display()))
    })
}

fn agent_from_view(
    view: &RunView,
    worker_id: Option<String>,
    capability_policy: super::super::CapabilityPolicy,
) -> RunReportAgent {
    RunReportAgent {
        agent_id: format!("run:{}", view.run.run_id),
        worker_id,
        run_id: Some(view.run.run_id.clone()),
        session_id: view.run.session_id.clone(),
        parent_agent_id: view
            .run
            .parent_run_id
            .as_ref()
            .map(|id| format!("run:{id}")),
        status: view.run.status.clone(),
        task: view.run.task.clone(),
        started_at: nonempty(&view.run.started_at),
        finished_at: view.run.finished_at.clone(),
        duration_ms: view.run.duration_ms,
        usage: view.usage.clone(),
        visible_output: view.visible_text.clone(),
        execution: view
            .metadata
            .execution
            .as_ref()
            .map(RunReportExecution::from),
        capability_policy: Some(capability_policy),
        mutation_scope: None,
        approval_policy: None,
    }
}

fn session_pointers_consistent(
    parent_view: &RunView,
    child: &super::RunChildRecord,
    child_view: Option<&RunView>,
) -> Option<bool> {
    let mut comparisons = Vec::new();
    if let (Some(actual), Some(declared)) = (
        parent_view.run.session_id.as_deref(),
        child.parent_session_id.as_deref(),
    ) {
        comparisons.push(actual == declared);
    }
    if let (Some(actual), Some(child_view)) = (parent_view.run.session_id.as_deref(), child_view) {
        if let Some(declared) = child_view.run.parent_session_id.as_deref() {
            comparisons.push(actual == declared);
        }
    }
    if let (Some(declared), Some(child_view)) = (child.session_id.as_deref(), child_view) {
        if let Some(actual) = child_view.run.session_id.as_deref() {
            comparisons.push(actual == declared);
        }
    }
    (!comparisons.is_empty()).then(|| comparisons.into_iter().all(|matches| matches))
}

fn agent_from_child(child: &super::RunChildRecord, parent_run_id: &str) -> RunReportAgent {
    let policy = current_policy();
    RunReportAgent {
        agent_id: child
            .run_id
            .as_ref()
            .map(|run_id| format!("run:{run_id}"))
            .unwrap_or_else(|| format!("worker:{}", child.worker_id)),
        worker_id: Some(child.worker_id.clone()),
        run_id: child.run_id.clone(),
        session_id: child.session_id.clone(),
        parent_agent_id: Some(format!("run:{parent_run_id}")),
        status: child.status.clone(),
        task: policy.redact_string(&child.task).into_owned(),
        started_at: nonempty(&child.started_at),
        finished_at: child.finished_at.clone(),
        execution: child.execution.as_ref().map(RunReportExecution::from),
        capability_policy: None,
        mutation_scope: child.mutation_scope.clone(),
        approval_policy: child.approval_policy.clone(),
        ..RunReportAgent::default()
    }
}

fn llm_call_from_span(run_id: &str, span: &super::RunTraceSpanRecord) -> RunReportLlmCall {
    let integer = |key: &str| span.metadata.get(key).and_then(Value::as_i64);
    let number = |key: &str| span.metadata.get(key).and_then(Value::as_f64);
    let text = |key: &str| {
        span.metadata
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    RunReportLlmCall {
        agent_id: format!("run:{run_id}"),
        call_id: format!("{}:{}", span.trace_id, span.span_id),
        provider: text(crate::tracing::meta::PROVIDER),
        model: text(crate::tracing::meta::MODEL),
        start_ms: span.start_ms,
        duration_ms: span.duration_ms,
        ttft_ms: span.ttft_ms,
        input_tokens: integer(crate::tracing::meta::INPUT_TOKENS),
        output_tokens: integer(crate::tracing::meta::OUTPUT_TOKENS),
        cache_read_tokens: integer(crate::tracing::meta::CACHE_READ_TOKENS),
        cache_write_tokens: integer(crate::tracing::meta::CACHE_WRITE_TOKENS),
        cost_usd: span
            .cost_usd
            .or_else(|| number(crate::tracing::meta::COST_USD)),
    }
}

fn coordination_summary(
    delegations: &[RunReportDelegation],
    checks: &[RunReportCheck],
    join_evidence: &JoinEvidenceProjection,
) -> RunReportCoordination {
    let terminal = delegations
        .iter()
        .filter(|delegation| {
            crate::agent_events::WorkerEvent::status_is_terminal(&delegation.status)
        })
        .count();
    let intervals: Vec<(i128, i128)> = delegations
        .iter()
        .filter_map(|delegation| {
            let start = parse_timestamp_ms(delegation.started_at.as_deref()?)?;
            let finish = parse_timestamp_ms(delegation.finished_at.as_deref()?)?;
            (finish >= start).then_some((start, finish))
        })
        .collect();
    let mut points = intervals
        .iter()
        .flat_map(|(start, finish)| [(*start, 1_i32), (*finish, -1_i32)])
        .collect::<Vec<_>>();
    // Millisecond/second persistence can collapse a short run to a zero-length
    // interval. Apply starts before finishes at the same timestamp so an
    // observed child still contributes to the peak instead of disappearing.
    points.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));
    let max_concurrent_children = if delegations.is_empty() {
        Some(0)
    } else if intervals.len() == delegations.len() {
        Some(
            points
                .into_iter()
                .fold((0_i32, 0_i32), |(active, max), (_, delta)| {
                    let active = active + delta;
                    (active, max.max(active))
                })
                .1
                .max(0) as usize,
        )
    } else {
        None
    };
    RunReportCoordination {
        spawned: delegations.len(),
        terminal,
        open: delegations.len().saturating_sub(terminal),
        orphaned: checks
            .iter()
            .filter(|check| check.code == "parent_run_missing")
            .count(),
        unjoined: join_evidence.complete.then(|| {
            delegations
                .iter()
                .filter(|delegation| {
                    crate::agent_events::WorkerEvent::status_is_terminal(&delegation.status)
                        && !join_evidence.joined(delegation)
                })
                .count()
        }),
        max_concurrent_children,
        observed_wait_ms: if join_evidence.complete {
            join_evidence.max_wait_ms
        } else {
            None
        },
        observed_join_ms: if join_evidence.complete {
            join_evidence.max_terminal_to_collection_ms
        } else {
            None
        },
        observed_result_processing_ms: if join_evidence.complete {
            join_evidence.max_result_processing_ms
        } else {
            None
        },
    }
}

fn source_for_snapshot(
    id: String,
    kind: &str,
    path: &Path,
    sha256: &str,
    source_root: Option<&Path>,
    agent_id: Option<String>,
) -> RunReportSource {
    RunReportSource {
        id,
        agent_id,
        kind: kind.to_string(),
        path: Some(display_path(path, source_root)),
        sha256: Some(sha256.to_string()),
        status: "verified".to_string(),
        error: None,
    }
}

fn display_path(path: &Path, source_root: Option<&Path>) -> String {
    let normalized = canonicalize_with_missing_suffix(path).unwrap_or_else(|| path.to_path_buf());
    source_root
        .and_then(|root| normalized.strip_prefix(root).ok())
        .unwrap_or(&normalized)
        .to_string_lossy()
        .into_owned()
}

fn canonicalize_with_missing_suffix(path: &Path) -> Option<PathBuf> {
    let mut cursor = path;
    let mut suffix = Vec::new();
    while !cursor.exists() {
        suffix.push(cursor.file_name()?.to_os_string());
        cursor = cursor.parent()?;
    }
    let mut normalized = cursor.canonicalize().ok()?;
    for component in suffix.into_iter().rev() {
        normalized.push(component);
    }
    Some(normalized)
}

fn check(code: &str, severity: &str, agent_id: &str, message: String) -> RunReportCheck {
    RunReportCheck {
        code: code.to_string(),
        severity: severity.to_string(),
        status: "failed".to_string(),
        agent_id: Some(agent_id.to_string()),
        message,
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{save_run_record, RunChildRecord, RunTraceSpanRecord};
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("harn-{label}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn report_correlates_parent_and_child_bidirectionally() {
        let dir = temp_dir("run-report-lineage");
        let parent_path = dir.join("parent.json");
        let child_path = dir.join("child.json");
        let child = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "child".to_string(),
            workflow_id: "child-workflow".to_string(),
            task: "child task".to_string(),
            status: "completed".to_string(),
            started_at: "2026-08-02T10:00:01Z".to_string(),
            finished_at: Some("2026-08-02T10:00:03Z".to_string()),
            parent_run_id: Some("parent".to_string()),
            root_run_id: Some("parent".to_string()),
            metadata: BTreeMap::from([
                (
                    "session_id".to_string(),
                    Value::String("child-session".to_string()),
                ),
                (
                    "parent_session_id".to_string(),
                    Value::String("parent-session".to_string()),
                ),
            ]),
            ..RunRecord::default()
        };
        let parent = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "parent".to_string(),
            workflow_id: "parent-workflow".to_string(),
            task: "parent task".to_string(),
            status: "completed".to_string(),
            started_at: "2026-08-02T10:00:00Z".to_string(),
            finished_at: Some("2026-08-02T10:00:04Z".to_string()),
            root_run_id: Some("parent".to_string()),
            child_runs: vec![RunChildRecord {
                worker_id: "worker-1".to_string(),
                worker_name: "child".to_string(),
                task: "child task".to_string(),
                status: "completed".to_string(),
                started_at: "2026-08-02T10:00:01Z".to_string(),
                finished_at: Some("2026-08-02T10:00:03Z".to_string()),
                session_id: Some("child-session".to_string()),
                parent_session_id: Some("parent-session".to_string()),
                run_id: Some("child".to_string()),
                run_path: Some(child_path.to_string_lossy().into_owned()),
                ..RunChildRecord::default()
            }],
            metadata: BTreeMap::from([(
                "session_id".to_string(),
                Value::String("parent-session".to_string()),
            )]),
            ..RunRecord::default()
        };
        save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
        save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: parent_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();

        assert_eq!(report.agents.len(), 2);
        assert_eq!(report.delegations.len(), 1);
        assert!(report.delegations[0].forward_pointer);
        assert!(report.delegations[0].back_pointer);
        assert_eq!(report.delegations[0].session_pointer_consistent, Some(true));
        assert_eq!(report.coordination.max_concurrent_children, Some(1));
        assert_eq!(report.coordination.unjoined, None);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "coordination_timing_unavailable"));
        assert!(report.projection.hash.starts_with("sha256:"));
        assert_eq!(
            report
                .sources
                .iter()
                .map(|source| source.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            report.sources.len()
        );
        assert!(
            report.sources.iter().all(|source| {
                source
                    .path
                    .as_deref()
                    .is_none_or(|path| !Path::new(path).is_absolute())
            }),
            "sources={:?}",
            report.sources
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn report_flags_missing_child_record_without_inventing_a_join() {
        let dir = temp_dir("run-report-missing-child");
        let parent_path = dir.join("parent.json");
        let parent = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "parent".to_string(),
            workflow_id: "parent-workflow".to_string(),
            status: "completed".to_string(),
            child_runs: vec![RunChildRecord {
                worker_id: "worker-1".to_string(),
                worker_name: "child".to_string(),
                status: "running".to_string(),
                run_id: Some("missing-child".to_string()),
                run_path: Some(dir.join("missing.json").to_string_lossy().into_owned()),
                ..RunChildRecord::default()
            }],
            ..RunRecord::default()
        };
        save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: parent_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();

        assert_eq!(report.coordination.open, 1);
        assert_eq!(report.coordination.unjoined, None);
        assert!(report
            .agents
            .iter()
            .any(|agent| agent.agent_id == "run:missing-child"));
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "child_run_missing"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn suspended_child_is_open_until_resumed_or_stopped() {
        let coordination = coordination_summary(
            &[RunReportDelegation {
                status: "suspended".to_string(),
                ..RunReportDelegation::default()
            }],
            &[],
            &JoinEvidenceProjection::default(),
        );

        assert_eq!(coordination.terminal, 0);
        assert_eq!(coordination.open, 1);
    }

    #[tokio::test]
    async fn report_recovers_missing_forward_run_id_from_child_path() {
        let dir = temp_dir("run-report-path-lineage");
        let parent_path = dir.join("parent.json");
        let child_path = dir.join("child.json");
        let child = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "child".to_string(),
            workflow_id: "child-workflow".to_string(),
            status: "completed".to_string(),
            parent_run_id: Some("parent".to_string()),
            started_at: uuid::Uuid::now_v7().to_string(),
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
            ..RunRecord::default()
        };
        let parent = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "parent".to_string(),
            workflow_id: "parent-workflow".to_string(),
            status: "completed".to_string(),
            child_runs: vec![RunChildRecord {
                worker_id: "worker-1".to_string(),
                status: "running".to_string(),
                run_id: None,
                run_path: Some(child_path.to_string_lossy().into_owned()),
                started_at: child.started_at.clone(),
                finished_at: child.finished_at.clone(),
                ..RunChildRecord::default()
            }],
            ..RunRecord::default()
        };
        save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
        save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: parent_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();

        assert_eq!(report.agents.len(), 2);
        assert_eq!(report.delegations.len(), 1);
        assert_eq!(report.delegations[0].child_agent_id, "run:child");
        assert_eq!(report.delegations[0].status, "completed");
        assert_eq!(report.delegations[0].parent_observed_status, "running");
        assert_eq!(report.coordination.terminal, 1);
        assert_eq!(report.coordination.max_concurrent_children, Some(1));
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "child_run_id_missing"));
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "child_status_mismatch"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn report_flags_timeline_truncation_before_a_late_llm_call() {
        let dir = temp_dir("run-report-timeline-truncation");
        let run_path = dir.join("run.json");
        let mut trace_spans = (1..=1025)
            .map(|span_id| RunTraceSpanRecord {
                trace_id: "trace-root".to_string(),
                span_id,
                kind: "import".to_string(),
                name: format!("import-{span_id}"),
                start_ms: span_id,
                duration_ms: 1,
                ..RunTraceSpanRecord::default()
            })
            .collect::<Vec<_>>();
        trace_spans.push(RunTraceSpanRecord {
            trace_id: "trace-root".to_string(),
            span_id: 1026,
            kind: "llm_call".to_string(),
            name: "late-llm-call".to_string(),
            start_ms: 1026,
            duration_ms: 2,
            ..RunTraceSpanRecord::default()
        });
        let run = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "root".to_string(),
            status: "completed".to_string(),
            trace_spans,
            ..RunRecord::default()
        };
        save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: run_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();

        assert_eq!(report.timelines.len(), 1);
        let timeline = &report.timelines[0];
        assert_eq!(timeline.coverage.returned, 1024);
        assert_eq!(timeline.coverage.available, Some(1026));
        assert!(timeline.coverage.truncated);
        assert!(!timeline.nodes.iter().any(|node| node.kind == "llm_call"));
        assert_eq!(report.llm_calls.len(), 1);
        let check = report
            .checks
            .iter()
            .find(|check| check.code == "timeline_truncated")
            .expect("explicit truncation check");
        assert_eq!(check.severity, "warning");
        assert!(check.message.contains("1024 of 1026 available"));
        assert!(check.message.contains("absence must not be inferred"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn report_redacts_returned_projection_and_hash_is_reproducible() {
        let dir = temp_dir("run-report-redaction");
        let run_path = dir.join("run.json");
        let secret = "sk-proj-test-abcdefghijklmnopqrstuvwxyz123456";
        let run = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "root".to_string(),
            workflow_id: "workflow".to_string(),
            task: format!("do not expose {secret}"),
            status: "completed".to_string(),
            transcript: Some(serde_json::json!({
                "events": [{
                    "kind": "message",
                    "role": "assistant",
                    "visibility": "public",
                    "blocks": [
                        {"type": "output_text", "text": format!("safe answer {secret}"), "visibility": "public"},
                        {"type": "reasoning", "text": "private chain of thought", "visibility": "private"}
                    ]
                }]
            })),
            ..RunRecord::default()
        };
        fs::write(&run_path, serde_json::to_vec(&run).unwrap()).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: run_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();
        let rendered = serde_json::to_string(&report).unwrap();
        assert!(!rendered.contains(secret));
        let visible = report.agents[0]
            .visible_output
            .as_deref()
            .expect("public transcript output");
        assert!(visible.starts_with("safe answer "));
        assert!(visible.contains("<redacted:openai_key:"));
        assert!(!rendered.contains("private chain of thought"));

        let expected_hash = report.projection.hash.clone();
        let mut value = serde_json::to_value(&report).unwrap();
        value["projection"]["hash"] = Value::String(String::new());
        let actual_hash = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(crate::canonical_json::to_vec(&value)))
        );
        assert_eq!(actual_hash, expected_hash);

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_report_does_not_follow_implicit_sidecar_symlinks() {
        let dir = temp_dir("run-report-sidecar-symlink");
        let outside = temp_dir("run-report-sidecar-outside");
        let run_path = dir.join("root.json");
        let run = RunRecord {
            type_name: "workflow_run".to_string(),
            id: "root".to_string(),
            workflow_id: "workflow".to_string(),
            status: "completed".to_string(),
            ..RunRecord::default()
        };
        fs::write(&run_path, serde_json::to_vec(&run).unwrap()).unwrap();
        fs::write(
            outside.join("llm_transcript.jsonl"),
            "{\"type\":\"daemon_event\",\"secret\":\"outside\"}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("root-llm")).unwrap();

        let report = build_run_report(RunReportRequest {
            run_record_path: run_path,
            allowed_roots: vec![dir.clone()],
            source_root: Some(dir.clone()),
            ..RunReportRequest::default()
        })
        .await
        .unwrap();

        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].kind, "run_record");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
