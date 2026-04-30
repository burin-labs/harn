//! Run records, replay fixtures, eval reports, and diff utilities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    default_run_dir, evaluate_context_pack_suggestion_expectations,
    generate_context_pack_suggestions, new_id, normalize_friction_events_json, now_rfc3339,
    parse_json_payload, parse_json_value, sync_run_handoffs, ArtifactRecord, CapabilityPolicy,
    ContextPackSuggestionExpectation, ContextPackSuggestionOptions, FrictionEvent, HandoffArtifact,
};
use crate::event_log::{
    active_event_log, AnyEventLog, EventLog, LogEvent as EventLogRecord, Topic,
};
use crate::llm::vm_value_to_json;
use crate::triggers::{SignatureStatus, TriggerEvent};
use crate::value::{VmError, VmValue};

pub const ACTION_GRAPH_NODE_KIND_RUN: &str = "run";
pub const ACTION_GRAPH_NODE_KIND_TRIGGER: &str = "trigger";
pub const ACTION_GRAPH_NODE_KIND_PREDICATE: &str = "predicate";
pub const ACTION_GRAPH_NODE_KIND_TRIGGER_PREDICATE: &str = "trigger_predicate";
pub const ACTION_GRAPH_NODE_KIND_STAGE: &str = "stage";
pub const ACTION_GRAPH_NODE_KIND_WORKER: &str = "worker";
pub const ACTION_GRAPH_NODE_KIND_DISPATCH: &str = "dispatch";
pub const ACTION_GRAPH_NODE_KIND_A2A_HOP: &str = "a2a_hop";
pub const ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE: &str = "worker_enqueue";
pub const ACTION_GRAPH_NODE_KIND_RETRY: &str = "retry";
pub const ACTION_GRAPH_NODE_KIND_DLQ: &str = "dlq";

pub const ACTION_GRAPH_EDGE_KIND_ENTRY: &str = "entry";
pub const ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH: &str = "trigger_dispatch";
pub const ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH: &str = "a2a_dispatch";
pub const ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE: &str = "predicate_gate";
pub const ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN: &str = "replay_chain";
pub const ACTION_GRAPH_EDGE_KIND_TRANSITION: &str = "transition";
pub const ACTION_GRAPH_EDGE_KIND_DELEGATES: &str = "delegates";
pub const ACTION_GRAPH_EDGE_KIND_RETRY: &str = "retry";
pub const ACTION_GRAPH_EDGE_KIND_DLQ_MOVE: &str = "dlq_move";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmUsageRecord {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_duration_ms: i64,
    pub call_count: i64,
    pub total_cost: f64,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageRecord {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub visible_text: Option<String>,
    pub private_reasoning: Option<String>,
    pub transcript: Option<serde_json::Value>,
    pub verification: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
    pub attempts: Vec<RunStageAttemptRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageAttemptRecord {
    pub attempt: usize,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub error: Option<String>,
    pub verification: Option<serde_json::Value>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTransitionRecord {
    pub id: String,
    pub from_stage_id: Option<String>,
    pub from_node_id: Option<String>,
    pub to_node_id: String,
    pub branch: Option<String>,
    pub timestamp: String,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunCheckpointRecord {
    pub id: String,
    pub ready_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub last_stage_id: Option<String>,
    pub persisted_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayFixture {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub source_run_id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub created_at: String,
    pub eval_kind: Option<String>,
    pub clarifying_question: Option<ClarifyingQuestionEvalSpec>,
    pub expected_status: String,
    pub stage_assertions: Vec<ReplayStageAssertion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ClarifyingQuestionEvalSpec {
    pub expected_question: Option<String>,
    pub accepted_questions: Vec<String>,
    pub required_terms: Vec<String>,
    pub forbidden_terms: Vec<String>,
    pub min_questions: usize,
    pub max_questions: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayStageAssertion {
    pub node_id: String,
    pub expected_status: String,
    pub expected_outcome: String,
    pub expected_branch: Option<String>,
    pub required_artifact_kinds: Vec<String>,
    pub visible_text_contains: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalReport {
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalCaseReport {
    pub run_id: String,
    pub workflow_id: String,
    pub label: Option<String>,
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
    pub source_path: Option<String>,
    pub comparison: Option<RunDiffReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalSuiteReport {
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub cases: Vec<ReplayEvalCaseReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunDeliverableSummaryRecord {
    pub id: String,
    pub text: String,
    pub status: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTaskLedgerSummaryRecord {
    pub root_task: String,
    pub rationale: String,
    pub deliverables: Vec<RunDeliverableSummaryRecord>,
    pub observations: Vec<String>,
    pub blocking_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunPlannerRoundRecord {
    pub stage_id: String,
    pub node_id: String,
    pub stage_kind: String,
    pub status: String,
    pub outcome: String,
    pub iteration_count: usize,
    pub llm_call_count: usize,
    pub tool_execution_count: usize,
    pub tool_rejection_count: usize,
    pub intervention_count: usize,
    pub compaction_count: usize,
    pub native_text_tool_fallback_count: usize,
    pub native_text_tool_fallback_rejection_count: usize,
    pub empty_completion_retry_count: usize,
    pub tools_used: Vec<String>,
    pub successful_tools: Vec<String>,
    pub ledger_done_rejections: usize,
    pub task_ledger: Option<RunTaskLedgerSummaryRecord>,
    pub research_facts: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunWorkerLineageRecord {
    pub worker_id: String,
    pub worker_name: String,
    pub parent_stage_id: Option<String>,
    pub task: String,
    pub status: String,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
    pub snapshot_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunActionGraphNodeRecord {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub status: String,
    pub outcome: String,
    pub trace_id: Option<String>,
    pub stage_id: Option<String>,
    pub node_id: Option<String>,
    pub worker_id: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunActionGraphEdgeRecord {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunVerificationOutcomeRecord {
    pub stage_id: String,
    pub node_id: String,
    pub status: String,
    pub passed: Option<bool>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTranscriptPointerRecord {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub location: String,
    pub path: Option<String>,
    pub available: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CompactionEventRecord {
    pub id: String,
    pub transcript_id: Option<String>,
    pub stage_id: Option<String>,
    pub node_id: Option<String>,
    pub mode: String,
    pub strategy: String,
    pub archived_messages: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub snapshot_asset_id: Option<String>,
    pub snapshot_location: String,
    pub snapshot_path: Option<String>,
    pub available: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DaemonEventKindRecord {
    #[default]
    Spawned,
    Triggered,
    Snapshotted,
    Resumed,
    Stopped,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DaemonEventRecord {
    pub daemon_id: String,
    pub name: String,
    pub kind: DaemonEventKindRecord,
    pub timestamp: String,
    pub persist_path: String,
    pub payload_summary: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunObservabilityRecord {
    pub schema_version: usize,
    pub planner_rounds: Vec<RunPlannerRoundRecord>,
    pub research_fact_count: usize,
    pub action_graph_nodes: Vec<RunActionGraphNodeRecord>,
    pub action_graph_edges: Vec<RunActionGraphEdgeRecord>,
    pub worker_lineage: Vec<RunWorkerLineageRecord>,
    pub verification_outcomes: Vec<RunVerificationOutcomeRecord>,
    pub transcript_pointers: Vec<RunTranscriptPointerRecord>,
    pub compaction_events: Vec<CompactionEventRecord>,
    pub daemon_events: Vec<DaemonEventRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunStageDiffRecord {
    pub node_id: String,
    pub change: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolCallDiffRecord {
    pub tool_name: String,
    pub args_hash: String,
    pub result_changed: bool,
    pub left_result: Option<String>,
    pub right_result: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunObservabilityDiffRecord {
    pub section: String,
    pub label: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunDiffReport {
    pub left_run_id: String,
    pub right_run_id: String,
    pub identical: bool,
    pub status_changed: bool,
    pub left_status: String,
    pub right_status: String,
    pub stage_diffs: Vec<RunStageDiffRecord>,
    pub tool_diffs: Vec<ToolCallDiffRecord>,
    pub observability_diffs: Vec<RunObservabilityDiffRecord>,
    pub transition_count_delta: isize,
    pub artifact_count_delta: isize,
    pub checkpoint_count_delta: isize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub base_dir: Option<String>,
    pub cases: Vec<EvalSuiteCase>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteCase {
    pub label: Option<String>,
    pub run_path: String,
    pub fixture_path: Option<String>,
    pub compare_to: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackManifest {
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub base_dir: Option<String>,
    pub baseline: Option<String>,
    pub package: Option<EvalPackPackage>,
    pub defaults: EvalPackDefaults,
    pub fixtures: Vec<EvalPackFixtureRef>,
    pub rubrics: Vec<EvalPackRubric>,
    pub judge: Option<EvalPackJudgeConfig>,
    pub cases: Vec<EvalPackCase>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackPackage {
    pub name: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub templates: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackDefaults {
    pub severity: Option<String>,
    pub fixture_root: Option<String>,
    pub thresholds: EvalPackThresholds,
    pub judge: Option<EvalPackJudgeConfig>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackFixtureRef {
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    #[serde(default, alias = "trace-id")]
    pub trace_id: Option<String>,
    pub provider: Option<String>,
    #[serde(default, alias = "event-kind")]
    pub event_kind: Option<String>,
    pub inline: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackRubric {
    pub id: String,
    pub kind: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub assertions: Vec<EvalPackAssertion>,
    pub judge: Option<EvalPackJudgeConfig>,
    pub calibration: Vec<EvalPackGoldenExample>,
    pub thresholds: EvalPackThresholds,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackAssertion {
    pub kind: String,
    pub stage: Option<String>,
    pub path: Option<String>,
    pub op: Option<String>,
    pub expected: Option<serde_json::Value>,
    pub contains: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackJudgeConfig {
    pub model: Option<String>,
    #[serde(default, alias = "prompt-version")]
    pub prompt_version: Option<String>,
    #[serde(default, alias = "tie-break")]
    pub tie_break: Option<String>,
    #[serde(default, alias = "confidence-min")]
    pub confidence_min: Option<f64>,
    pub temperature: Option<f64>,
    pub rubric: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackGoldenExample {
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub score: Option<f64>,
    pub explanation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackThresholds {
    pub severity: Option<String>,
    #[serde(default, alias = "min-score")]
    pub min_score: Option<f64>,
    #[serde(default, alias = "min-confidence")]
    pub min_confidence: Option<f64>,
    #[serde(default, alias = "max-cost-usd")]
    pub max_cost_usd: Option<f64>,
    #[serde(default, alias = "max-latency-ms")]
    pub max_latency_ms: Option<i64>,
    #[serde(default, alias = "max-tokens")]
    pub max_tokens: Option<i64>,
    #[serde(default, alias = "max-stage-count")]
    pub max_stage_count: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackCase {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub run: Option<String>,
    #[serde(default, alias = "run-path")]
    pub run_path: Option<String>,
    #[serde(default, alias = "friction-events", alias = "friction_events")]
    pub friction_events: Option<String>,
    pub fixture: Option<String>,
    #[serde(default, alias = "fixture-path")]
    pub fixture_path: Option<String>,
    #[serde(default, alias = "compare-to")]
    pub compare_to: Option<String>,
    pub rubrics: Vec<String>,
    pub severity: Option<String>,
    pub thresholds: EvalPackThresholds,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackReport {
    pub pack_id: String,
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub blocking_failed: usize,
    pub warning_failed: usize,
    pub informational_failed: usize,
    pub cases: Vec<EvalPackCaseReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackCaseReport {
    pub id: String,
    pub label: String,
    pub severity: String,
    pub pass: bool,
    pub blocking: bool,
    pub run_id: String,
    pub workflow_id: String,
    pub source_path: Option<String>,
    pub stage_count: usize,
    pub failures: Vec<String>,
    pub warnings: Vec<String>,
    pub informational: Vec<String>,
    pub comparison: Option<RunDiffReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunHitlQuestionRecord {
    pub request_id: String,
    pub prompt: String,
    pub agent: String,
    pub trace_id: Option<String>,
    pub asked_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub parent_run_id: Option<String>,
    pub root_run_id: Option<String>,
    pub stages: Vec<RunStageRecord>,
    pub transitions: Vec<RunTransitionRecord>,
    pub checkpoints: Vec<RunCheckpointRecord>,
    pub pending_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub child_runs: Vec<RunChildRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub handoffs: Vec<HandoffArtifact>,
    pub policy: CapabilityPolicy,
    pub execution: Option<RunExecutionRecord>,
    pub transcript: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub replay_fixture: Option<ReplayFixture>,
    pub observability: Option<RunObservabilityRecord>,
    pub trace_spans: Vec<RunTraceSpanRecord>,
    pub tool_recordings: Vec<ToolCallRecord>,
    pub hitl_questions: Vec<RunHitlQuestionRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub persisted_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub tool_use_id: String,
    pub args_hash: String,
    pub result: String,
    pub is_rejected: bool,
    pub duration_ms: u64,
    pub iteration: usize,
    pub timestamp: String,
}

/// Hash a tool invocation for fixture lookup (name + canonical args JSON).
pub fn tool_fixture_hash(tool_name: &str, args: &serde_json::Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    let args_str = serde_json::to_string(args).unwrap_or_default();
    args_str.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTraceSpanRecord {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: String,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunChildRecord {
    pub worker_id: String,
    pub worker_name: String,
    pub parent_stage_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub mutation_scope: Option<String>,
    pub approval_policy: Option<super::ToolApprovalPolicy>,
    pub task: String,
    pub request: Option<serde_json::Value>,
    pub provenance: Option<serde_json::Value>,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
    pub snapshot_path: Option<String>,
    pub execution: Option<RunExecutionRecord>,
}

pub(crate) fn run_child_record_from_worker_metadata(
    parent_stage_id: Option<String>,
    worker: &serde_json::Value,
) -> Option<RunChildRecord> {
    let worker_id = worker.get("id").and_then(|value| value.as_str())?;
    if worker_id.is_empty() {
        return None;
    }
    Some(RunChildRecord {
        worker_id: worker_id.to_string(),
        worker_name: worker
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("worker")
            .to_string(),
        parent_stage_id,
        session_id: worker
            .get("audit")
            .and_then(|value| value.get("session_id"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        parent_session_id: worker
            .get("audit")
            .and_then(|value| value.get("parent_session_id"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        mutation_scope: worker
            .get("audit")
            .and_then(|value| value.get("mutation_scope"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        approval_policy: worker
            .get("audit")
            .and_then(|value| value.get("approval_policy"))
            .and_then(|value| {
                serde_json::from_value::<super::ToolApprovalPolicy>(value.clone()).ok()
            }),
        task: worker
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        request: worker.get("request").cloned(),
        provenance: worker.get("provenance").cloned(),
        status: worker
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("completed")
            .to_string(),
        started_at: worker
            .get("started_at")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        finished_at: worker
            .get("finished_at")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        run_id: worker
            .get("child_run_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        run_path: worker
            .get("child_run_path")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        snapshot_path: worker
            .get("snapshot_path")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        execution: worker
            .get("execution")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    })
}

fn run_child_from_stage_metadata(stage: &RunStageRecord) -> Option<RunChildRecord> {
    let parent_stage_id = if stage.id.is_empty() {
        None
    } else {
        Some(stage.id.clone())
    };
    run_child_record_from_worker_metadata(parent_stage_id, stage.metadata.get("worker")?)
}

fn fill_missing_child_run_fields(existing: &mut RunChildRecord, child: RunChildRecord) {
    if existing.worker_name.is_empty() {
        existing.worker_name = child.worker_name;
    }
    if existing.parent_stage_id.is_none() {
        existing.parent_stage_id = child.parent_stage_id;
    }
    if existing.session_id.is_none() {
        existing.session_id = child.session_id;
    }
    if existing.parent_session_id.is_none() {
        existing.parent_session_id = child.parent_session_id;
    }
    if existing.mutation_scope.is_none() {
        existing.mutation_scope = child.mutation_scope;
    }
    if existing.approval_policy.is_none() {
        existing.approval_policy = child.approval_policy;
    }
    if existing.task.is_empty() {
        existing.task = child.task;
    }
    if existing.request.is_none() {
        existing.request = child.request;
    }
    if existing.provenance.is_none() {
        existing.provenance = child.provenance;
    }
    if existing.status.is_empty() {
        existing.status = child.status;
    }
    if existing.started_at.is_empty() {
        existing.started_at = child.started_at;
    }
    if existing.finished_at.is_none() {
        existing.finished_at = child.finished_at;
    }
    if existing.run_id.is_none() {
        existing.run_id = child.run_id;
    }
    if existing.run_path.is_none() {
        existing.run_path = child.run_path;
    }
    if existing.snapshot_path.is_none() {
        existing.snapshot_path = child.snapshot_path;
    }
    if existing.execution.is_none() {
        existing.execution = child.execution;
    }
}

fn materialize_child_runs_from_stage_metadata(run: &mut RunRecord) {
    for child in run
        .stages
        .iter()
        .filter_map(run_child_from_stage_metadata)
        .collect::<Vec<_>>()
    {
        match run
            .child_runs
            .iter_mut()
            .find(|existing| existing.worker_id == child.worker_id)
        {
            Some(existing) => fill_missing_child_run_fields(existing, child),
            None => run.child_runs.push(child),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunExecutionRecord {
    pub cwd: Option<String>,
    pub source_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub adapter: Option<String>,
    pub repo_path: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub cleanup: Option<String>,
}

fn compact_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn normalize_question_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clarifying_min_questions(spec: &ClarifyingQuestionEvalSpec) -> usize {
    spec.min_questions.max(1)
}

fn clarifying_max_questions(spec: &ClarifyingQuestionEvalSpec) -> usize {
    spec.max_questions.unwrap_or(1).max(1)
}

fn read_topic_records(
    log: &AnyEventLog,
    topic: &Topic,
) -> Vec<(crate::event_log::EventId, EventLogRecord)> {
    let mut from = None;
    let mut records = Vec::new();
    loop {
        let batch =
            futures::executor::block_on(log.read_range(topic, from, 256)).unwrap_or_default();
        if batch.is_empty() {
            break;
        }
        from = batch.last().map(|(event_id, _)| *event_id);
        records.extend(batch);
    }
    records
}

fn merge_hitl_questions_from_active_log(run: &mut RunRecord) {
    let Some(log) = active_event_log() else {
        return;
    };
    let topic = Topic::new(crate::HITL_QUESTIONS_TOPIC)
        .expect("static hitl.questions topic should always be valid");
    let mut merged = run
        .hitl_questions
        .iter()
        .cloned()
        .map(|question| (question.request_id.clone(), question))
        .collect::<BTreeMap<_, _>>();

    for (_, event) in read_topic_records(log.as_ref(), &topic) {
        if event.kind != "hitl.question_asked" {
            continue;
        }
        let payload = &event.payload;
        let matches_run = event
            .headers
            .get("run_id")
            .is_some_and(|value| value == &run.id)
            || payload
                .get("run_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == run.id);
        if !matches_run {
            continue;
        }
        let request_id = payload
            .get("request_id")
            .and_then(|value| value.as_str())
            .or_else(|| event.headers.get("request_id").map(String::as_str))
            .unwrap_or_default();
        let prompt = payload
            .get("payload")
            .and_then(|value| value.get("prompt"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if request_id.is_empty() || prompt.is_empty() {
            continue;
        }
        merged.insert(
            request_id.to_string(),
            RunHitlQuestionRecord {
                request_id: request_id.to_string(),
                prompt: prompt.to_string(),
                agent: payload
                    .get("agent")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                trace_id: payload
                    .get("trace_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                asked_at: payload
                    .get("requested_at")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
            },
        );
    }

    run.hitl_questions = merged.into_values().collect();
    run.hitl_questions.sort_by(|left, right| {
        (left.asked_at.as_str(), left.request_id.as_str())
            .cmp(&(right.asked_at.as_str(), right.request_id.as_str()))
    });
}

fn signature_status_label(status: &SignatureStatus) -> &'static str {
    match status {
        SignatureStatus::Verified => "verified",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Failed { .. } => "failed",
    }
}

fn trigger_event_from_run(run: &RunRecord) -> Option<TriggerEvent> {
    run.metadata
        .get("trigger_event")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn run_trace_id(run: &RunRecord, trigger_event: Option<&TriggerEvent>) -> Option<String> {
    trigger_event
        .map(|event| event.trace_id.0.clone())
        .or_else(|| {
            run.metadata
                .get("trace_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
}

fn replay_of_event_id_from_run(run: &RunRecord) -> Option<String> {
    run.metadata
        .get("replay_of_event_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn action_graph_kind_for_stage(stage: &RunStageRecord) -> &'static str {
    if stage.kind == "condition" {
        ACTION_GRAPH_NODE_KIND_PREDICATE
    } else {
        ACTION_GRAPH_NODE_KIND_STAGE
    }
}

fn trigger_node_metadata(trigger_event: &TriggerEvent) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "provider".to_string(),
        serde_json::json!(trigger_event.provider.as_str()),
    );
    metadata.insert(
        "event_kind".to_string(),
        serde_json::json!(trigger_event.kind),
    );
    metadata.insert(
        "dedupe_key".to_string(),
        serde_json::json!(trigger_event.dedupe_key),
    );
    metadata.insert(
        "signature_status".to_string(),
        serde_json::json!(signature_status_label(&trigger_event.signature_status)),
    );
    metadata
}

fn stage_node_metadata(stage: &RunStageRecord) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert("stage_kind".to_string(), serde_json::json!(stage.kind));
    if let Some(branch) = stage.branch.as_ref() {
        metadata.insert("branch".to_string(), serde_json::json!(branch));
    }
    if let Some(worker_id) = stage
        .metadata
        .get("worker_id")
        .and_then(|value| value.as_str())
    {
        metadata.insert("worker_id".to_string(), serde_json::json!(worker_id));
    }
    metadata
}

fn append_action_graph_node(
    nodes: &mut Vec<RunActionGraphNodeRecord>,
    record: RunActionGraphNodeRecord,
) {
    nodes.push(record);
}

pub async fn append_action_graph_update(
    headers: BTreeMap<String, String>,
    payload: serde_json::Value,
) -> Result<(), crate::event_log::LogError> {
    let Some(log) = active_event_log() else {
        return Ok(());
    };
    let topic = Topic::new("observability.action_graph")
        .expect("static observability.action_graph topic should always be valid");
    let record = EventLogRecord::new("action_graph_update", payload).with_headers(headers);
    log.append(&topic, record).await.map(|_| ())
}

fn publish_action_graph_event(
    run: &RunRecord,
    observability: &RunObservabilityRecord,
    path: &Path,
) {
    let trigger_event = trigger_event_from_run(run);
    let mut headers = BTreeMap::new();
    headers.insert("run_id".to_string(), run.id.clone());
    headers.insert("workflow_id".to_string(), run.workflow_id.clone());
    if let Some(trace_id) = run_trace_id(run, trigger_event.as_ref()) {
        headers.insert("trace_id".to_string(), trace_id);
    }
    let payload = serde_json::json!({
        "run_id": run.id,
        "workflow_id": run.workflow_id,
        "persisted_path": path.to_string_lossy(),
        "status": run.status,
        "observability": observability,
    });
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = append_action_graph_update(headers, payload).await;
        });
    } else {
        let _ = futures::executor::block_on(append_action_graph_update(headers, payload));
    }
}

fn llm_transcript_sidecar_path(run_path: &Path) -> Option<PathBuf> {
    let stem = run_path.file_stem()?.to_str()?;
    let parent = run_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}-llm/llm_transcript.jsonl")))
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn json_usize(value: Option<&serde_json::Value>) -> usize {
    value.and_then(|value| value.as_u64()).unwrap_or_default() as usize
}

fn json_bool(value: Option<&serde_json::Value>) -> Option<bool> {
    value.and_then(|value| value.as_bool())
}

fn stage_result_payload(stage: &RunStageRecord) -> Option<&serde_json::Value> {
    stage
        .artifacts
        .iter()
        .find_map(|artifact| artifact.data.as_ref())
}

fn task_ledger_summary_from_value(value: &serde_json::Value) -> Option<RunTaskLedgerSummaryRecord> {
    let deliverables = value
        .get("deliverables")
        .and_then(|raw| raw.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| RunDeliverableSummaryRecord {
                    id: item
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    text: item
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    status: item
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    note: item
                        .get("note")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let observations = json_string_array(value.get("observations"));
    let root_task = value
        .get("root_task")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let rationale = value
        .get("rationale")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if root_task.is_empty()
        && rationale.is_empty()
        && deliverables.is_empty()
        && observations.is_empty()
    {
        return None;
    }
    let blocking_count = deliverables
        .iter()
        .filter(|deliverable| matches!(deliverable.status.as_str(), "open" | "blocked"))
        .count();
    Some(RunTaskLedgerSummaryRecord {
        root_task,
        rationale,
        deliverables,
        observations,
        blocking_count,
    })
}

fn compaction_events_from_transcript(
    transcript: &serde_json::Value,
    stage_id: Option<&str>,
    node_id: Option<&str>,
    location_prefix: &str,
    persisted_path: Option<&Path>,
) -> Vec<CompactionEventRecord> {
    let transcript_id = transcript
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let asset_ids = transcript
        .get("assets")
        .and_then(|value| value.as_array())
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| {
                    asset
                        .get("id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    transcript
        .get("events")
        .and_then(|value| value.as_array())
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    event.get("kind").and_then(|value| value.as_str()) == Some("compaction")
                })
                .map(|event| {
                    let metadata = event.get("metadata");
                    let snapshot_asset_id = metadata
                        .and_then(|value| value.get("snapshot_asset_id"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let available = snapshot_asset_id
                        .as_ref()
                        .is_some_and(|asset_id| asset_ids.contains(asset_id));
                    let snapshot_location = snapshot_asset_id
                        .as_ref()
                        .map(|asset_id| format!("{location_prefix}.assets[{asset_id}]"))
                        .unwrap_or_else(|| location_prefix.to_string());
                    CompactionEventRecord {
                        id: event
                            .get("id")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        transcript_id: transcript_id.clone(),
                        stage_id: stage_id.map(str::to_string),
                        node_id: node_id.map(str::to_string),
                        mode: metadata
                            .and_then(|value| value.get("mode"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        strategy: metadata
                            .and_then(|value| value.get("strategy"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        archived_messages: json_usize(
                            metadata.and_then(|value| value.get("archived_messages")),
                        ),
                        estimated_tokens_before: json_usize(
                            metadata.and_then(|value| value.get("estimated_tokens_before")),
                        ),
                        estimated_tokens_after: json_usize(
                            metadata.and_then(|value| value.get("estimated_tokens_after")),
                        ),
                        snapshot_asset_id,
                        snapshot_location,
                        snapshot_path: persisted_path
                            .map(|path| path.to_string_lossy().into_owned()),
                        available,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn daemon_events_from_sidecar(run_path: &Path) -> Vec<DaemonEventRecord> {
    let Some(sidecar_path) = llm_transcript_sidecar_path(run_path) else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(sidecar_path) else {
        return Vec::new();
    };

    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event.get("type").and_then(|value| value.as_str()) == Some("daemon_event"))
        .filter_map(|event| serde_json::from_value::<DaemonEventRecord>(event).ok())
        .collect()
}

pub fn derive_run_observability(
    run: &RunRecord,
    persisted_path: Option<&Path>,
) -> RunObservabilityRecord {
    let mut action_graph_nodes = Vec::new();
    let mut action_graph_edges = Vec::new();
    let mut verification_outcomes = Vec::new();
    let mut planner_rounds = Vec::new();
    let mut transcript_pointers = Vec::new();
    let mut compaction_events = Vec::new();
    let mut daemon_events = Vec::new();
    let mut research_fact_count = 0usize;

    let root_node_id = format!("run:{}", run.id);
    let trigger_event = trigger_event_from_run(run);
    let propagated_trace_id = run_trace_id(run, trigger_event.as_ref());
    append_action_graph_node(
        &mut action_graph_nodes,
        RunActionGraphNodeRecord {
            id: root_node_id.clone(),
            label: run
                .workflow_name
                .clone()
                .unwrap_or_else(|| run.workflow_id.clone()),
            kind: ACTION_GRAPH_NODE_KIND_RUN.to_string(),
            status: run.status.clone(),
            outcome: run.status.clone(),
            trace_id: propagated_trace_id.clone(),
            stage_id: None,
            node_id: None,
            worker_id: None,
            run_id: Some(run.id.clone()),
            run_path: run.persisted_path.clone(),
            metadata: BTreeMap::from([(
                "workflow_id".to_string(),
                serde_json::json!(run.workflow_id),
            )]),
        },
    );
    let mut entry_node_id = root_node_id.clone();
    if let Some(trigger_event) = trigger_event.as_ref() {
        if let Some(replay_of_event_id) = replay_of_event_id_from_run(run) {
            let replay_source_node_id = format!("trigger:{replay_of_event_id}");
            append_action_graph_node(
                &mut action_graph_nodes,
                RunActionGraphNodeRecord {
                    id: replay_source_node_id.clone(),
                    label: format!(
                        "{}:{} (original {})",
                        trigger_event.provider.as_str(),
                        trigger_event.kind,
                        replay_of_event_id
                    ),
                    kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
                    status: "historical".to_string(),
                    outcome: "replayed_from".to_string(),
                    trace_id: Some(trigger_event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: Some(run.id.clone()),
                    run_path: run.persisted_path.clone(),
                    metadata: trigger_node_metadata(trigger_event),
                },
            );
            action_graph_edges.push(RunActionGraphEdgeRecord {
                from_id: replay_source_node_id,
                to_id: format!("trigger:{}", trigger_event.id.0),
                kind: ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN.to_string(),
                label: Some("replay chain".to_string()),
            });
        }
        let trigger_node_id = format!("trigger:{}", trigger_event.id.0);
        append_action_graph_node(
            &mut action_graph_nodes,
            RunActionGraphNodeRecord {
                id: trigger_node_id.clone(),
                label: format!("{}:{}", trigger_event.provider.as_str(), trigger_event.kind),
                kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
                status: "received".to_string(),
                outcome: signature_status_label(&trigger_event.signature_status).to_string(),
                trace_id: Some(trigger_event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: Some(run.id.clone()),
                run_path: run.persisted_path.clone(),
                metadata: trigger_node_metadata(trigger_event),
            },
        );
        action_graph_edges.push(RunActionGraphEdgeRecord {
            from_id: root_node_id.clone(),
            to_id: trigger_node_id.clone(),
            kind: ACTION_GRAPH_EDGE_KIND_ENTRY.to_string(),
            label: Some(trigger_event.id.0.clone()),
        });
        entry_node_id = trigger_node_id;
    }

    let stage_node_ids = run
        .stages
        .iter()
        .map(|stage| (stage.id.clone(), format!("stage:{}", stage.id)))
        .collect::<BTreeMap<_, _>>();
    let stage_by_id = run
        .stages
        .iter()
        .map(|stage| (stage.id.as_str(), stage))
        .collect::<BTreeMap<_, _>>();
    let stage_by_node_id = run
        .stages
        .iter()
        .map(|stage| (stage.node_id.clone(), format!("stage:{}", stage.id)))
        .collect::<BTreeMap<_, _>>();

    let incoming_nodes = run
        .transitions
        .iter()
        .map(|transition| transition.to_node_id.clone())
        .collect::<BTreeSet<_>>();

    for stage in &run.stages {
        let graph_node_id = stage_node_ids
            .get(&stage.id)
            .cloned()
            .unwrap_or_else(|| format!("stage:{}", stage.id));
        append_action_graph_node(
            &mut action_graph_nodes,
            RunActionGraphNodeRecord {
                id: graph_node_id.clone(),
                label: stage.node_id.clone(),
                kind: action_graph_kind_for_stage(stage).to_string(),
                status: stage.status.clone(),
                outcome: stage.outcome.clone(),
                trace_id: propagated_trace_id.clone(),
                stage_id: Some(stage.id.clone()),
                node_id: Some(stage.node_id.clone()),
                worker_id: stage
                    .metadata
                    .get("worker_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                run_id: None,
                run_path: None,
                metadata: stage_node_metadata(stage),
            },
        );
        if !incoming_nodes.contains(&stage.node_id) {
            action_graph_edges.push(RunActionGraphEdgeRecord {
                from_id: entry_node_id.clone(),
                to_id: graph_node_id.clone(),
                kind: if trigger_event.is_some() {
                    ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH.to_string()
                } else {
                    ACTION_GRAPH_EDGE_KIND_ENTRY.to_string()
                },
                label: None,
            });
        }

        if stage.kind == "verify" || stage.verification.is_some() {
            let passed = json_bool(
                stage
                    .verification
                    .as_ref()
                    .and_then(|value| value.get("pass")),
            )
            .or_else(|| {
                json_bool(
                    stage
                        .verification
                        .as_ref()
                        .and_then(|value| value.get("success")),
                )
            })
            .or_else(|| {
                if stage.status == "completed" && stage.outcome == "success" {
                    Some(true)
                } else if stage.status == "failed" || stage.outcome == "failed" {
                    Some(false)
                } else {
                    None
                }
            });
            verification_outcomes.push(RunVerificationOutcomeRecord {
                stage_id: stage.id.clone(),
                node_id: stage.node_id.clone(),
                status: stage.status.clone(),
                passed,
                summary: stage
                    .verification
                    .as_ref()
                    .map(compact_json_value)
                    .or_else(|| {
                        stage
                            .visible_text
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .cloned()
                    }),
            });
        }

        if stage.transcript.is_some() {
            transcript_pointers.push(RunTranscriptPointerRecord {
                id: format!("stage:{}:transcript", stage.id),
                label: format!("Stage {} transcript", stage.node_id),
                kind: "embedded_transcript".to_string(),
                location: format!("run.stages[{}].transcript", stage.node_id),
                path: run.persisted_path.clone(),
                available: true,
            });
            if let Some(transcript) = stage.transcript.as_ref() {
                compaction_events.extend(compaction_events_from_transcript(
                    transcript,
                    Some(&stage.id),
                    Some(&stage.node_id),
                    &format!("run.stages[{}].transcript", stage.node_id),
                    persisted_path,
                ));
            }
        }

        if let Some(payload) = stage_result_payload(stage) {
            let trace = payload.get("trace");
            let task_ledger = payload
                .get("task_ledger")
                .and_then(task_ledger_summary_from_value);
            let research_facts = task_ledger
                .as_ref()
                .map(|ledger| ledger.observations.clone())
                .unwrap_or_default();
            research_fact_count += research_facts.len();
            let tools_payload = payload.get("tools");
            let tools_used = json_string_array(
                tools_payload
                    .and_then(|tools| tools.get("calls"))
                    .or_else(|| trace.and_then(|trace| trace.get("tools_used"))),
            );
            let successful_tools =
                json_string_array(tools_payload.and_then(|tools| tools.get("successful")));
            let planner_round = RunPlannerRoundRecord {
                stage_id: stage.id.clone(),
                node_id: stage.node_id.clone(),
                stage_kind: stage.kind.clone(),
                status: stage.status.clone(),
                outcome: stage.outcome.clone(),
                iteration_count: json_usize(trace.and_then(|trace| trace.get("iterations"))),
                llm_call_count: json_usize(trace.and_then(|trace| trace.get("llm_calls"))),
                tool_execution_count: json_usize(
                    trace.and_then(|trace| trace.get("tool_executions")),
                ),
                tool_rejection_count: json_usize(
                    trace.and_then(|trace| trace.get("tool_rejections")),
                ),
                intervention_count: json_usize(trace.and_then(|trace| trace.get("interventions"))),
                compaction_count: json_usize(trace.and_then(|trace| trace.get("compactions"))),
                native_text_tool_fallback_count: json_usize(
                    trace.and_then(|trace| trace.get("native_text_tool_fallbacks")),
                ),
                native_text_tool_fallback_rejection_count: json_usize(
                    trace.and_then(|trace| trace.get("native_text_tool_fallback_rejections")),
                ),
                empty_completion_retry_count: json_usize(
                    trace.and_then(|trace| trace.get("empty_completion_retries")),
                ),
                tools_used,
                successful_tools,
                ledger_done_rejections: json_usize(payload.get("ledger_done_rejections")),
                task_ledger,
                research_facts,
            };
            let has_agentic_detail = planner_round.iteration_count > 0
                || planner_round.llm_call_count > 0
                || planner_round.tool_execution_count > 0
                || planner_round.native_text_tool_fallback_count > 0
                || planner_round.native_text_tool_fallback_rejection_count > 0
                || planner_round.empty_completion_retry_count > 0
                || planner_round.ledger_done_rejections > 0
                || planner_round.task_ledger.is_some()
                || !planner_round.tools_used.is_empty()
                || !planner_round.successful_tools.is_empty();
            if has_agentic_detail {
                planner_rounds.push(planner_round);
            }
        }
    }

    for transition in &run.transitions {
        let Some(to_id) = stage_by_node_id.get(&transition.to_node_id).cloned() else {
            continue;
        };
        let from_stage = transition
            .from_stage_id
            .as_deref()
            .and_then(|stage_id| stage_by_id.get(stage_id).copied());
        let from_id = transition
            .from_stage_id
            .as_ref()
            .and_then(|stage_id| stage_node_ids.get(stage_id))
            .cloned()
            .or_else(|| {
                transition
                    .from_node_id
                    .as_ref()
                    .and_then(|node_id| stage_by_node_id.get(node_id))
                    .cloned()
            })
            .unwrap_or_else(|| root_node_id.clone());
        action_graph_edges.push(RunActionGraphEdgeRecord {
            from_id,
            to_id,
            kind: if from_stage.is_some_and(|stage| stage.kind == "condition") {
                ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE.to_string()
            } else {
                ACTION_GRAPH_EDGE_KIND_TRANSITION.to_string()
            },
            label: transition.branch.clone(),
        });
    }

    let worker_lineage = run
        .child_runs
        .iter()
        .map(|child| {
            let worker_node_id = format!("worker:{}", child.worker_id);
            append_action_graph_node(
                &mut action_graph_nodes,
                RunActionGraphNodeRecord {
                    id: worker_node_id.clone(),
                    label: child.worker_name.clone(),
                    kind: ACTION_GRAPH_NODE_KIND_WORKER.to_string(),
                    status: child.status.clone(),
                    outcome: child.status.clone(),
                    trace_id: propagated_trace_id.clone(),
                    stage_id: child.parent_stage_id.clone(),
                    node_id: None,
                    worker_id: Some(child.worker_id.clone()),
                    run_id: child.run_id.clone(),
                    run_path: child.run_path.clone(),
                    metadata: BTreeMap::from([
                        (
                            "worker_name".to_string(),
                            serde_json::json!(child.worker_name),
                        ),
                        ("task".to_string(), serde_json::json!(child.task)),
                    ]),
                },
            );
            if let Some(parent_stage_id) = child.parent_stage_id.as_ref() {
                if let Some(stage_node_id) = stage_node_ids.get(parent_stage_id) {
                    action_graph_edges.push(RunActionGraphEdgeRecord {
                        from_id: stage_node_id.clone(),
                        to_id: worker_node_id,
                        kind: ACTION_GRAPH_EDGE_KIND_DELEGATES.to_string(),
                        label: Some(child.worker_name.clone()),
                    });
                }
            }
            RunWorkerLineageRecord {
                worker_id: child.worker_id.clone(),
                worker_name: child.worker_name.clone(),
                parent_stage_id: child.parent_stage_id.clone(),
                task: child.task.clone(),
                status: child.status.clone(),
                session_id: child.session_id.clone(),
                parent_session_id: child.parent_session_id.clone(),
                run_id: child.run_id.clone(),
                run_path: child.run_path.clone(),
                snapshot_path: child.snapshot_path.clone(),
            }
        })
        .collect::<Vec<_>>();

    if run.transcript.is_some() {
        transcript_pointers.push(RunTranscriptPointerRecord {
            id: "run:transcript".to_string(),
            label: "Run transcript".to_string(),
            kind: "embedded_transcript".to_string(),
            location: "run.transcript".to_string(),
            path: run.persisted_path.clone(),
            available: true,
        });
        if let Some(transcript) = run.transcript.as_ref() {
            compaction_events.extend(compaction_events_from_transcript(
                transcript,
                None,
                None,
                "run.transcript",
                persisted_path,
            ));
        }
    }

    if let Some(path) = persisted_path {
        if let Some(sidecar_path) = llm_transcript_sidecar_path(path) {
            transcript_pointers.push(RunTranscriptPointerRecord {
                id: "run:llm_transcript".to_string(),
                label: "LLM transcript sidecar".to_string(),
                kind: "llm_jsonl".to_string(),
                location: "run sidecar".to_string(),
                path: Some(sidecar_path.to_string_lossy().into_owned()),
                available: sidecar_path.exists(),
            });
        }
        daemon_events.extend(daemon_events_from_sidecar(path));
    }

    RunObservabilityRecord {
        schema_version: 4,
        planner_rounds,
        research_fact_count,
        action_graph_nodes,
        action_graph_edges,
        worker_lineage,
        verification_outcomes,
        transcript_pointers,
        compaction_events,
        daemon_events,
    }
}

fn refresh_run_observability(run: &mut RunRecord, persisted_path: Option<&Path>) {
    run.observability = Some(derive_run_observability(run, persisted_path));
}

pub fn normalize_run_record(value: &VmValue) -> Result<RunRecord, VmError> {
    let mut run: RunRecord = parse_json_payload(vm_value_to_json(value), "run_record")?;
    if run.type_name.is_empty() {
        run.type_name = "run_record".to_string();
    }
    if run.id.is_empty() {
        run.id = new_id("run");
    }
    if run.started_at.is_empty() {
        run.started_at = now_rfc3339();
    }
    if run.status.is_empty() {
        run.status = "running".to_string();
    }
    if run.root_run_id.is_none() {
        run.root_run_id = Some(run.id.clone());
    }
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    merge_hitl_questions_from_active_log(&mut run);
    materialize_child_runs_from_stage_metadata(&mut run);
    sync_run_handoffs(&mut run);
    if run.observability.is_none() {
        let persisted_path = run.persisted_path.clone();
        let persisted = persisted_path.as_deref().map(Path::new);
        refresh_run_observability(&mut run, persisted);
    }
    Ok(run)
}

pub fn normalize_eval_suite_manifest(value: &VmValue) -> Result<EvalSuiteManifest, VmError> {
    let mut manifest: EvalSuiteManifest = parse_json_value(value)?;
    if manifest.type_name.is_empty() {
        manifest.type_name = "eval_suite_manifest".to_string();
    }
    if manifest.id.is_empty() {
        manifest.id = new_id("eval_suite");
    }
    Ok(manifest)
}

pub fn load_eval_suite_manifest(path: &Path) -> Result<EvalSuiteManifest, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read eval suite manifest: {e}")))?;
    let mut manifest: EvalSuiteManifest = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse eval suite manifest: {e}")))?;
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn load_eval_pack_manifest(path: &Path) -> Result<EvalPackManifest, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read eval pack manifest: {e}")))?;
    let mut manifest: EvalPackManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            serde_json::from_str(&content)
                .map_err(|e| VmError::Runtime(format!("failed to parse eval pack JSON: {e}")))?
        } else {
            toml::from_str(&content)
                .map_err(|e| VmError::Runtime(format!("failed to parse eval pack TOML: {e}")))?
        };
    normalize_eval_pack_manifest(&mut manifest);
    if manifest.base_dir.is_none() {
        manifest.base_dir = path.parent().map(|parent| parent.display().to_string());
    }
    Ok(manifest)
}

pub fn normalize_eval_pack_manifest_value(value: &VmValue) -> Result<EvalPackManifest, VmError> {
    let mut manifest: EvalPackManifest = parse_json_value(value)?;
    normalize_eval_pack_manifest(&mut manifest);
    Ok(manifest)
}

fn normalize_eval_pack_manifest(manifest: &mut EvalPackManifest) {
    if manifest.version == 0 {
        manifest.version = 1;
    }
    if manifest.id.is_empty() {
        manifest.id = manifest
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| new_id("eval_pack"));
    }
}

fn load_replay_fixture(path: &Path) -> Result<ReplayFixture, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read replay fixture: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse replay fixture: {e}")))
}

fn load_run_record_from_fixture_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<RunRecord, VmError> {
    if let Some(inline) = &fixture.inline {
        let run: RunRecord = serde_json::from_value(inline.clone())
            .map_err(|e| VmError::Runtime(format!("failed to parse inline run record: {e}")))?;
        return Ok(run);
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline run",
            fixture.id
        ))
    })?;
    load_run_record(&resolve_manifest_path(base_dir, path))
}

fn load_replay_fixture_from_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<ReplayFixture, VmError> {
    if let Some(inline) = &fixture.inline {
        return serde_json::from_value(inline.clone())
            .map_err(|e| VmError::Runtime(format!("failed to parse inline replay fixture: {e}")));
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline replay fixture",
            fixture.id
        ))
    })?;
    load_replay_fixture(&resolve_manifest_path(base_dir, path))
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        path_buf
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path_buf)
    } else {
        path_buf
    }
}

pub fn evaluate_run_suite_manifest(
    manifest: &EvalSuiteManifest,
) -> Result<ReplayEvalSuiteReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let mut reports = Vec::new();
    for case in &manifest.cases {
        let run_path = resolve_manifest_path(base_dir, &case.run_path);
        let run = load_run_record(&run_path)?;
        let fixture = match &case.fixture_path {
            Some(path) => load_replay_fixture(&resolve_manifest_path(base_dir, path))?,
            None => run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| replay_fixture_from_run(&run)),
        };
        let eval = evaluate_run_against_fixture(&run, &fixture);
        let mut pass = eval.pass;
        let mut failures = eval.failures;
        let comparison = match &case.compare_to {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    pass = false;
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: case.label.clone(),
            pass,
            failures,
            stage_count: eval.stage_count,
            source_path: Some(run_path.display().to_string()),
            comparison,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    Ok(ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    })
}

pub fn evaluate_eval_pack_manifest(manifest: &EvalPackManifest) -> Result<EvalPackReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let fixture_base_dir_buf = manifest
        .defaults
        .fixture_root
        .as_deref()
        .map(|root| resolve_manifest_path(base_dir, root));
    let fixture_base_dir = fixture_base_dir_buf.as_deref().or(base_dir);
    let fixtures_by_id: BTreeMap<&str, &EvalPackFixtureRef> = manifest
        .fixtures
        .iter()
        .filter(|fixture| !fixture.id.is_empty())
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect();
    let rubrics_by_id: BTreeMap<&str, &EvalPackRubric> = manifest
        .rubrics
        .iter()
        .filter(|rubric| !rubric.id.is_empty())
        .map(|rubric| (rubric.id.as_str(), rubric))
        .collect();

    let mut reports = Vec::new();
    for (index, case) in manifest.cases.iter().enumerate() {
        let case_id = case
            .id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| format!("case_{}", index + 1));
        let label = case
            .name
            .clone()
            .or_else(|| case.id.clone())
            .unwrap_or_else(|| case_id.clone());
        let severity = eval_pack_case_severity(manifest, case);
        let blocking = severity == "blocking";
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let mut informational = Vec::new();

        if case.friction_events.is_some() {
            let report = evaluate_eval_pack_friction_case(
                manifest,
                case,
                &case_id,
                &label,
                &severity,
                blocking,
                base_dir,
                fixture_base_dir,
                &fixtures_by_id,
                &rubrics_by_id,
            )?;
            reports.push(report);
            continue;
        }

        let run = load_eval_pack_case_run(case, base_dir, fixture_base_dir, &fixtures_by_id)?;
        let fixture =
            load_eval_pack_case_fixture(case, base_dir, fixture_base_dir, &fixtures_by_id, &run)?;
        let eval = evaluate_run_against_fixture(&run, &fixture);
        failures.extend(eval.failures);
        apply_eval_pack_thresholds(&run, &manifest.defaults.thresholds, &mut failures);
        apply_eval_pack_thresholds(&run, &case.thresholds, &mut failures);

        let comparison = match case.compare_to.as_ref().or(manifest.baseline.as_ref()) {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };

        for rubric_id in &case.rubrics {
            let Some(rubric) = rubrics_by_id.get(rubric_id.as_str()) else {
                failures.push(format!("case references unknown rubric '{rubric_id}'"));
                continue;
            };
            apply_eval_pack_rubric(rubric, &run, &mut failures, &mut warnings);
        }

        let pass = failures.is_empty() || !blocking;
        if !failures.is_empty() && !blocking {
            if severity == "warning" {
                warnings.append(&mut failures);
            } else {
                informational.append(&mut failures);
            }
        }
        reports.push(EvalPackCaseReport {
            id: case_id,
            label,
            severity,
            pass,
            blocking,
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            source_path: eval_pack_case_source_path(
                case,
                base_dir,
                fixture_base_dir,
                &fixtures_by_id,
            ),
            stage_count: eval.stage_count,
            failures,
            warnings,
            informational,
            comparison,
        });
    }

    let total = reports.len();
    let blocking_failed = reports
        .iter()
        .filter(|report| report.blocking && !report.failures.is_empty())
        .count();
    let warning_failed = reports
        .iter()
        .filter(|report| !report.warnings.is_empty())
        .count();
    let informational_failed = reports
        .iter()
        .filter(|report| !report.informational.is_empty())
        .count();
    let passed = reports.iter().filter(|report| report.pass).count();
    Ok(EvalPackReport {
        pack_id: manifest.id.clone(),
        pass: blocking_failed == 0,
        total,
        passed,
        failed: total.saturating_sub(passed),
        blocking_failed,
        warning_failed,
        informational_failed,
        cases: reports,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_eval_pack_friction_case(
    manifest: &EvalPackManifest,
    case: &EvalPackCase,
    case_id: &str,
    label: &str,
    severity: &str,
    blocking: bool,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    rubrics_by_id: &BTreeMap<&str, &EvalPackRubric>,
) -> Result<EvalPackCaseReport, VmError> {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut informational = Vec::new();
    let events =
        load_eval_pack_case_friction_events(case, base_dir, fixture_base_dir, fixtures_by_id)?;
    let options = friction_suggestion_options(case, manifest);
    let suggestions = generate_context_pack_suggestions(&events, &options);

    for rubric_id in &case.rubrics {
        let Some(rubric) = rubrics_by_id.get(rubric_id.as_str()) else {
            failures.push(format!("case references unknown rubric '{rubric_id}'"));
            continue;
        };
        apply_eval_pack_friction_rubric(rubric, &suggestions, &mut failures, &mut warnings);
    }

    if case.rubrics.is_empty() && suggestions.is_empty() {
        failures.push("friction fixture produced no context-pack suggestions".to_string());
    }

    let pass = failures.is_empty() || !blocking;
    if !failures.is_empty() && !blocking {
        if severity == "warning" {
            warnings.append(&mut failures);
        } else {
            informational.append(&mut failures);
        }
    }

    Ok(EvalPackCaseReport {
        id: case_id.to_string(),
        label: label.to_string(),
        severity: severity.to_string(),
        pass,
        blocking,
        run_id: "friction_events".to_string(),
        workflow_id: String::new(),
        source_path: eval_pack_case_friction_source_path(
            case,
            base_dir,
            fixture_base_dir,
            fixtures_by_id,
        ),
        stage_count: events.len(),
        failures,
        warnings,
        informational,
        comparison: None,
    })
}

fn eval_pack_case_severity(manifest: &EvalPackManifest, case: &EvalPackCase) -> String {
    normalize_eval_pack_severity(
        case.severity
            .as_deref()
            .or(case.thresholds.severity.as_deref())
            .or(manifest.defaults.severity.as_deref())
            .or(manifest.defaults.thresholds.severity.as_deref())
            .unwrap_or("blocking"),
    )
}

fn normalize_eval_pack_severity(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "warn" | "warning" => "warning".to_string(),
        "info" | "informational" => "informational".to_string(),
        _ => "blocking".to_string(),
    }
}

fn load_eval_pack_case_run(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Result<RunRecord, VmError> {
    if let Some(run_ref) = case.run.as_deref().or(case.run_path.as_deref()) {
        if let Some(fixture) = fixtures_by_id.get(run_ref) {
            return load_run_record_from_fixture_ref(fixture, fixture_base_dir);
        }
        return load_run_record(&resolve_manifest_path(base_dir, run_ref));
    }
    Err(VmError::Runtime(
        "eval pack case is missing run or run_path".to_string(),
    ))
}

fn load_eval_pack_case_fixture(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
    run: &RunRecord,
) -> Result<ReplayFixture, VmError> {
    if let Some(fixture_ref) = case.fixture.as_deref().or(case.fixture_path.as_deref()) {
        if let Some(fixture) = fixtures_by_id.get(fixture_ref) {
            return load_replay_fixture_from_ref(fixture, fixture_base_dir);
        }
        return load_replay_fixture(&resolve_manifest_path(base_dir, fixture_ref));
    }
    Ok(run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| replay_fixture_from_run(run)))
}

fn eval_pack_case_source_path(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Option<String> {
    let run_ref = case.run.as_deref().or(case.run_path.as_deref())?;
    if let Some(fixture) = fixtures_by_id.get(run_ref) {
        return fixture.path.as_ref().map(|path| {
            resolve_manifest_path(fixture_base_dir, path)
                .display()
                .to_string()
        });
    }
    Some(
        resolve_manifest_path(base_dir, run_ref)
            .display()
            .to_string(),
    )
}

fn load_eval_pack_case_friction_events(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Result<Vec<FrictionEvent>, VmError> {
    let event_ref = case.friction_events.as_deref().ok_or_else(|| {
        VmError::Runtime("eval pack friction case is missing friction_events".to_string())
    })?;
    if let Some(fixture) = fixtures_by_id.get(event_ref) {
        return load_friction_events_from_fixture_ref(fixture, fixture_base_dir);
    }
    load_friction_events_from_path(&resolve_manifest_path(base_dir, event_ref))
}

fn load_friction_events_from_fixture_ref(
    fixture: &EvalPackFixtureRef,
    base_dir: Option<&Path>,
) -> Result<Vec<FrictionEvent>, VmError> {
    if let Some(inline) = &fixture.inline {
        return normalize_friction_events_json(inline.clone());
    }
    let path = fixture.path.as_deref().ok_or_else(|| {
        VmError::Runtime(format!(
            "fixture '{}' is missing path or inline friction events",
            fixture.id
        ))
    })?;
    load_friction_events_from_path(&resolve_manifest_path(base_dir, path))
}

fn load_friction_events_from_path(path: &Path) -> Result<Vec<FrictionEvent>, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read friction events fixture: {e}")))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse friction events fixture: {e}")))?;
    normalize_friction_events_json(value)
}

fn eval_pack_case_friction_source_path(
    case: &EvalPackCase,
    base_dir: Option<&Path>,
    fixture_base_dir: Option<&Path>,
    fixtures_by_id: &BTreeMap<&str, &EvalPackFixtureRef>,
) -> Option<String> {
    let event_ref = case.friction_events.as_deref()?;
    if let Some(fixture) = fixtures_by_id.get(event_ref) {
        return fixture.path.as_ref().map(|path| {
            resolve_manifest_path(fixture_base_dir, path)
                .display()
                .to_string()
        });
    }
    Some(
        resolve_manifest_path(base_dir, event_ref)
            .display()
            .to_string(),
    )
}

fn friction_suggestion_options(
    case: &EvalPackCase,
    manifest: &EvalPackManifest,
) -> ContextPackSuggestionOptions {
    let min_occurrences = case
        .metadata
        .get("min_occurrences")
        .or_else(|| manifest.metadata.get("min_occurrences"))
        .and_then(|value| value.as_u64())
        .unwrap_or(2) as usize;
    let owner = case
        .metadata
        .get("owner")
        .or_else(|| manifest.metadata.get("owner"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            manifest
                .package
                .as_ref()
                .and_then(|package| package.name.clone())
        });
    ContextPackSuggestionOptions {
        min_occurrences,
        owner,
    }
}

fn apply_eval_pack_thresholds(
    run: &RunRecord,
    thresholds: &EvalPackThresholds,
    failures: &mut Vec<String>,
) {
    if let Some(max_stage_count) = thresholds.max_stage_count {
        if run.stages.len() > max_stage_count {
            failures.push(format!(
                "stage count {} exceeds threshold {}",
                run.stages.len(),
                max_stage_count
            ));
        }
    }
    if let Some(max_latency_ms) = thresholds.max_latency_ms {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.total_duration_ms)
            .unwrap_or_default();
        if actual > max_latency_ms {
            failures.push(format!(
                "latency {actual}ms exceeds threshold {max_latency_ms}ms"
            ));
        }
    }
    if let Some(max_cost_usd) = thresholds.max_cost_usd {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.total_cost)
            .unwrap_or_default();
        if actual > max_cost_usd {
            failures.push(format!(
                "cost ${actual:.6} exceeds threshold ${max_cost_usd:.6}"
            ));
        }
    }
    if let Some(max_tokens) = thresholds.max_tokens {
        let actual = run
            .usage
            .as_ref()
            .map(|usage| usage.input_tokens + usage.output_tokens)
            .unwrap_or_default();
        if actual > max_tokens {
            failures.push(format!(
                "token count {actual} exceeds threshold {max_tokens}"
            ));
        }
    }
}

fn apply_eval_pack_rubric(
    rubric: &EvalPackRubric,
    run: &RunRecord,
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match rubric.kind.as_str() {
        "" | "deterministic" | "replay" | "budget" | "hitl" | "side-effect" => {
            apply_eval_pack_thresholds(run, &rubric.thresholds, failures);
            for assertion in &rubric.assertions {
                apply_eval_pack_assertion(rubric, assertion, run, failures);
            }
        }
        "llm-judge" | "llm_as_judge" | "judge" => {
            let severity = normalize_eval_pack_severity(
                rubric.thresholds.severity.as_deref().unwrap_or("blocking"),
            );
            let message = format!(
                "rubric '{}' requires an external LLM judge and was not run locally",
                rubric.id
            );
            if severity == "blocking" {
                failures.push(message);
            } else {
                warnings.push(message);
            }
        }
        other => warnings.push(format!(
            "rubric '{}' has unknown kind '{}' and was not run locally",
            rubric.id, other
        )),
    }
}

fn apply_eval_pack_friction_rubric(
    rubric: &EvalPackRubric,
    suggestions: &[super::ContextPackSuggestion],
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match rubric.kind.as_str() {
        "" | "deterministic" | "friction" | "context-pack-suggestion" => {
            let mut expectations = Vec::new();
            for assertion in &rubric.assertions {
                match assertion.kind.as_str() {
                    "context-pack-suggestion" | "context_pack_suggestion" | "suggestion" => {
                        let expectation = context_pack_expectation_from_assertion(assertion);
                        expectations.push(expectation);
                    }
                    other => failures.push(format!(
                        "rubric '{}' has unsupported friction assertion kind '{}'",
                        rubric.id, other
                    )),
                }
            }
            failures.extend(evaluate_context_pack_suggestion_expectations(
                suggestions,
                &expectations,
            ));
        }
        other => warnings.push(format!(
            "rubric '{}' has unknown friction kind '{}' and was not run locally",
            rubric.id, other
        )),
    }
}

fn context_pack_expectation_from_assertion(
    assertion: &EvalPackAssertion,
) -> ContextPackSuggestionExpectation {
    let expected = assertion
        .expected
        .as_ref()
        .and_then(|value| value.as_object());
    let expected_string = assertion.expected.as_ref().and_then(|value| value.as_str());
    ContextPackSuggestionExpectation {
        min_suggestions: expected
            .and_then(|map| map.get("min_suggestions"))
            .and_then(|value| value.as_u64())
            .map(|value| value as usize),
        recommended_artifact: expected
            .and_then(|map| map.get("recommended_artifact"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| expected_string.map(str::to_string)),
        title_contains: assertion.contains.clone().or_else(|| {
            expected
                .and_then(|map| map.get("title_contains"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        }),
        manifest_name_contains: expected
            .and_then(|map| map.get("manifest_name_contains"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        required_capability: expected
            .and_then(|map| map.get("required_capability"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        required_output_slot: expected
            .and_then(|map| map.get("required_output_slot"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
    }
}

fn apply_eval_pack_assertion(
    rubric: &EvalPackRubric,
    assertion: &EvalPackAssertion,
    run: &RunRecord,
    failures: &mut Vec<String>,
) {
    match assertion.kind.as_str() {
        "run-status" | "run_status" | "status" => {
            let expected = assertion.expected.as_ref().and_then(|value| value.as_str());
            if let Some(expected) = expected {
                if run.status != expected {
                    failures.push(format!(
                        "rubric '{}' expected run status {}, got {}",
                        rubric.id, expected, run.status
                    ));
                }
            }
        }
        "stage-status" | "stage_status" => {
            let Some(stage_id) = assertion.stage.as_deref() else {
                failures.push(format!(
                    "rubric '{}' stage-status assertion missing stage",
                    rubric.id
                ));
                return;
            };
            let expected = assertion.expected.as_ref().and_then(|value| value.as_str());
            let Some(expected) = expected else {
                failures.push(format!(
                    "rubric '{}' stage-status assertion missing expected string",
                    rubric.id
                ));
                return;
            };
            match run.stages.iter().find(|stage| stage.node_id == stage_id) {
                Some(stage) if stage.status == expected => {}
                Some(stage) => failures.push(format!(
                    "rubric '{}' expected stage {} status {}, got {}",
                    rubric.id, stage_id, expected, stage.status
                )),
                None => failures.push(format!(
                    "rubric '{}' expected stage {} to exist",
                    rubric.id, stage_id
                )),
            }
        }
        "visible-text-contains" | "visible_text_contains" => {
            let Some(needle) = assertion.contains.as_deref() else {
                failures.push(format!(
                    "rubric '{}' visible-text assertion missing contains",
                    rubric.id
                ));
                return;
            };
            let matched = match assertion.stage.as_deref() {
                Some(stage_id) => run
                    .stages
                    .iter()
                    .find(|stage| stage.node_id == stage_id)
                    .and_then(|stage| stage.visible_text.as_deref())
                    .is_some_and(|text| text.contains(needle)),
                None => run
                    .stages
                    .iter()
                    .filter_map(|stage| stage.visible_text.as_deref())
                    .any(|text| text.contains(needle)),
            };
            if !matched {
                failures.push(format!(
                    "rubric '{}' expected visible text to contain {:?}",
                    rubric.id, needle
                ));
            }
        }
        "hitl-question-contains" | "hitl_question_contains" => {
            let Some(needle) = assertion.contains.as_deref() else {
                failures.push(format!(
                    "rubric '{}' HITL assertion missing contains",
                    rubric.id
                ));
                return;
            };
            if !run
                .hitl_questions
                .iter()
                .any(|question| question.prompt.contains(needle))
            {
                failures.push(format!(
                    "rubric '{}' expected HITL question to contain {:?}",
                    rubric.id, needle
                ));
            }
        }
        "" => {}
        other => failures.push(format!(
            "rubric '{}' has unsupported assertion kind '{}'",
            rubric.id, other
        )),
    }
}

/// Edit operation in a diff sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffOp {
    Equal,
    Delete,
    Insert,
}

/// Compute the shortest edit script using Myers' O(nd) algorithm.
/// Returns a sequence of (DiffOp, line_index_in_before_or_after).
/// Time: O(nd) where d = edit distance. Space: O(d * n).
pub(crate) fn myers_diff(a: &[&str], b: &[&str]) -> Vec<(DiffOp, usize)> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    if n == 0 && m == 0 {
        return Vec::new();
    }
    if n == 0 {
        return (0..m as usize).map(|j| (DiffOp::Insert, j)).collect();
    }
    if m == 0 {
        return (0..n as usize).map(|i| (DiffOp::Delete, i)).collect();
    }

    let max_d = (n + m) as usize;
    let offset = max_d as isize;
    let v_size = 2 * max_d + 1;
    let mut v = vec![0isize; v_size];
    // trace[d] holds the `v` snapshot BEFORE step d ran — required for backtrack.
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=max_d as isize {
        trace.push(v.clone());
        let mut new_v = v.clone();
        for k in (-d..=d).step_by(2) {
            let ki = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            new_v[ki] = x;
            if x >= n && y >= m {
                let _ = new_v;
                break 'outer;
            }
        }
        v = new_v;
    }

    let mut ops: Vec<(DiffOp, usize)> = Vec::new();
    let mut x = n;
    let mut y = m;
    for d in (1..trace.len() as isize).rev() {
        let k = x - y;
        let v_prev = &trace[d as usize];
        let prev_k = if k == -d
            || (k != d && v_prev[(k - 1 + offset) as usize] < v_prev[(k + 1 + offset) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v_prev[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push((DiffOp::Equal, x as usize));
        }
        if prev_k < k {
            x -= 1;
            ops.push((DiffOp::Delete, x as usize));
        } else {
            y -= 1;
            ops.push((DiffOp::Insert, y as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push((DiffOp::Equal, x as usize));
    }
    ops.reverse();
    ops
}

pub fn render_unified_diff(path: Option<&str>, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let ops = myers_diff(&before_lines, &after_lines);

    let mut diff = String::new();
    let file = path.unwrap_or("artifact");
    diff.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
    for &(op, idx) in &ops {
        match op {
            DiffOp::Equal => diff.push_str(&format!(" {}\n", before_lines[idx])),
            DiffOp::Delete => diff.push_str(&format!("-{}\n", before_lines[idx])),
            DiffOp::Insert => diff.push_str(&format!("+{}\n", after_lines[idx])),
        }
    }
    diff
}

pub fn save_run_record(run: &RunRecord, path: Option<&str>) -> Result<String, VmError> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_run_dir().join(format!("{}.json", run.id)));
    let mut materialized = run.clone();
    merge_hitl_questions_from_active_log(&mut materialized);
    materialize_child_runs_from_stage_metadata(&mut materialized);
    if materialized.replay_fixture.is_none() {
        materialized.replay_fixture = Some(replay_fixture_from_run(&materialized));
    }
    materialized.persisted_path = Some(path.to_string_lossy().into_owned());
    sync_run_handoffs(&mut materialized);
    refresh_run_observability(&mut materialized, Some(&path));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("failed to create run directory: {e}")))?;
    }
    let json = serde_json::to_string_pretty(&materialized)
        .map_err(|e| VmError::Runtime(format!("failed to encode run record: {e}")))?;
    // Atomic write: .tmp then rename guards against partial writes on kill.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| VmError::Runtime(format!("failed to persist run record: {e}")))?;
    std::fs::rename(&tmp_path, &path).map_err(|e| {
        // Cross-device renames fail on some filesystems; best-effort direct write.
        let _ = std::fs::write(&path, &json);
        VmError::Runtime(format!("failed to finalize run record: {e}"))
    })?;
    if let Some(observability) = materialized.observability.as_ref() {
        publish_action_graph_event(&materialized, observability, &path);
    }
    Ok(path.to_string_lossy().into_owned())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    let mut run: RunRecord = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))?;
    materialize_child_runs_from_stage_metadata(&mut run);
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    run.persisted_path
        .get_or_insert_with(|| path.to_string_lossy().into_owned());
    sync_run_handoffs(&mut run);
    refresh_run_observability(&mut run, Some(path));
    Ok(run)
}

pub fn replay_fixture_from_run(run: &RunRecord) -> ReplayFixture {
    ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: new_id("fixture"),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        created_at: now_rfc3339(),
        eval_kind: Some("replay".to_string()),
        clarifying_question: None,
        expected_status: run.status.clone(),
        stage_assertions: run
            .stages
            .iter()
            .map(|stage| ReplayStageAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.status.clone(),
                expected_outcome: stage.outcome.clone(),
                expected_branch: stage.branch.clone(),
                required_artifact_kinds: stage
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.kind.clone())
                    .collect(),
                visible_text_contains: stage
                    .visible_text
                    .as_ref()
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(80).collect()),
            })
            .collect(),
    }
}

pub fn evaluate_run_against_fixture(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    if fixture.eval_kind.as_deref() == Some("clarifying_question") {
        return evaluate_clarifying_question(run, fixture);
    }
    let mut failures = Vec::new();
    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    let stages_by_id: BTreeMap<&str, &RunStageRecord> =
        run.stages.iter().map(|s| (s.node_id.as_str(), s)).collect();
    for assertion in &fixture.stage_assertions {
        let Some(stage) = stages_by_id.get(assertion.node_id.as_str()) else {
            failures.push(format!("missing stage {}", assertion.node_id));
            continue;
        };
        if stage.status != assertion.expected_status {
            failures.push(format!(
                "stage {} status mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_status, stage.status
            ));
        }
        if stage.outcome != assertion.expected_outcome {
            failures.push(format!(
                "stage {} outcome mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_outcome, stage.outcome
            ));
        }
        if stage.branch != assertion.expected_branch {
            failures.push(format!(
                "stage {} branch mismatch: expected {:?}, got {:?}",
                assertion.node_id, assertion.expected_branch, stage.branch
            ));
        }
        for required_kind in &assertion.required_artifact_kinds {
            if !stage
                .artifacts
                .iter()
                .any(|artifact| &artifact.kind == required_kind)
            {
                failures.push(format!(
                    "stage {} missing artifact kind {}",
                    assertion.node_id, required_kind
                ));
            }
        }
        if let Some(snippet) = &assertion.visible_text_contains {
            let actual = stage.visible_text.clone().unwrap_or_default();
            if !actual.contains(snippet) {
                failures.push(format!(
                    "stage {} visible text does not contain expected snippet {:?}",
                    assertion.node_id, snippet
                ));
            }
        }
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

fn evaluate_clarifying_question(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    let mut failures = Vec::new();
    let spec = fixture.clarifying_question.clone().unwrap_or_default();
    let min_questions = clarifying_min_questions(&spec);
    let max_questions = clarifying_max_questions(&spec);
    let questions = &run.hitl_questions;

    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    if questions.len() < min_questions {
        failures.push(format!(
            "expected at least {min_questions} clarifying question(s), got {}",
            questions.len()
        ));
    }
    if questions.len() > max_questions {
        failures.push(format!(
            "expected at most {max_questions} clarifying question(s), got {}",
            questions.len()
        ));
    }

    let normalized_expected = spec
        .expected_question
        .as_deref()
        .map(normalize_question_text);
    let normalized_accepted = spec
        .accepted_questions
        .iter()
        .map(|question| normalize_question_text(question))
        .collect::<Vec<_>>();
    let required_terms = spec
        .required_terms
        .iter()
        .map(|term| normalize_question_text(term))
        .collect::<Vec<_>>();
    let forbidden_terms = spec
        .forbidden_terms
        .iter()
        .map(|term| normalize_question_text(term))
        .collect::<Vec<_>>();

    let matched = questions.iter().any(|question| {
        let normalized = normalize_question_text(&question.prompt);
        let matches_expected = normalized_expected
            .as_ref()
            .is_none_or(|expected| &normalized == expected)
            && (normalized_accepted.is_empty()
                || normalized_accepted
                    .iter()
                    .any(|candidate| candidate == &normalized));
        let has_required_terms = required_terms
            .iter()
            .all(|term| normalized.contains(term.as_str()));
        let avoids_forbidden_terms = forbidden_terms
            .iter()
            .all(|term| !normalized.contains(term.as_str()));
        matches_expected && has_required_terms && avoids_forbidden_terms
    });

    if !questions.is_empty()
        && (!normalized_accepted.is_empty()
            || normalized_expected.is_some()
            || !required_terms.is_empty()
            || !forbidden_terms.is_empty())
        && !matched
    {
        failures.push(format!(
            "no clarifying question matched fixture; actual questions: {}",
            questions
                .iter()
                .map(|question| format!("{:?}", question.prompt))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

pub fn evaluate_run_suite(
    cases: Vec<(RunRecord, ReplayFixture, Option<String>)>,
) -> ReplayEvalSuiteReport {
    let mut reports = Vec::new();
    for (run, fixture, source_path) in cases {
        let report = evaluate_run_against_fixture(&run, &fixture);
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: None,
            pass: report.pass,
            failures: report.failures,
            stage_count: report.stage_count,
            source_path,
            comparison: None,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    }
}

pub fn diff_run_records(left: &RunRecord, right: &RunRecord) -> RunDiffReport {
    let mut stage_diffs = Vec::new();
    let mut all_node_ids = BTreeSet::new();
    let left_by_id: BTreeMap<&str, &RunStageRecord> = left
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    let right_by_id: BTreeMap<&str, &RunStageRecord> = right
        .stages
        .iter()
        .map(|s| (s.node_id.as_str(), s))
        .collect();
    all_node_ids.extend(left_by_id.keys().copied());
    all_node_ids.extend(right_by_id.keys().copied());

    for node_id in all_node_ids {
        let left_stage = left_by_id.get(node_id).copied();
        let right_stage = right_by_id.get(node_id).copied();
        match (left_stage, right_stage) {
            (Some(_), None) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "removed".to_string(),
                details: vec!["stage missing from right run".to_string()],
            }),
            (None, Some(_)) => stage_diffs.push(RunStageDiffRecord {
                node_id: node_id.to_string(),
                change: "added".to_string(),
                details: vec!["stage missing from left run".to_string()],
            }),
            (Some(left_stage), Some(right_stage)) => {
                let mut details = Vec::new();
                if left_stage.status != right_stage.status {
                    details.push(format!(
                        "status: {} -> {}",
                        left_stage.status, right_stage.status
                    ));
                }
                if left_stage.outcome != right_stage.outcome {
                    details.push(format!(
                        "outcome: {} -> {}",
                        left_stage.outcome, right_stage.outcome
                    ));
                }
                if left_stage.branch != right_stage.branch {
                    details.push(format!(
                        "branch: {:?} -> {:?}",
                        left_stage.branch, right_stage.branch
                    ));
                }
                if left_stage.produced_artifact_ids.len() != right_stage.produced_artifact_ids.len()
                {
                    details.push(format!(
                        "produced_artifacts: {} -> {}",
                        left_stage.produced_artifact_ids.len(),
                        right_stage.produced_artifact_ids.len()
                    ));
                }
                if left_stage.artifacts.len() != right_stage.artifacts.len() {
                    details.push(format!(
                        "artifact_records: {} -> {}",
                        left_stage.artifacts.len(),
                        right_stage.artifacts.len()
                    ));
                }
                if !details.is_empty() {
                    stage_diffs.push(RunStageDiffRecord {
                        node_id: node_id.to_string(),
                        change: "changed".to_string(),
                        details,
                    });
                }
            }
            (None, None) => {}
        }
    }

    let mut tool_diffs = Vec::new();
    let left_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = left
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let right_tools: std::collections::BTreeMap<(String, String), &ToolCallRecord> = right
        .tool_recordings
        .iter()
        .map(|r| ((r.tool_name.clone(), r.args_hash.clone()), r))
        .collect();
    let all_tool_keys: std::collections::BTreeSet<_> = left_tools
        .keys()
        .chain(right_tools.keys())
        .cloned()
        .collect();
    for key in &all_tool_keys {
        let l = left_tools.get(key);
        let r = right_tools.get(key);
        let result_changed = match (l, r) {
            (Some(a), Some(b)) => a.result != b.result,
            _ => true,
        };
        if result_changed {
            tool_diffs.push(ToolCallDiffRecord {
                tool_name: key.0.clone(),
                args_hash: key.1.clone(),
                result_changed,
                left_result: l.map(|t| t.result.clone()),
                right_result: r.map(|t| t.result.clone()),
            });
        }
    }

    let left_observability = left.observability.clone().unwrap_or_else(|| {
        derive_run_observability(left, left.persisted_path.as_deref().map(Path::new))
    });
    let right_observability = right.observability.clone().unwrap_or_else(|| {
        derive_run_observability(right, right.persisted_path.as_deref().map(Path::new))
    });
    let mut observability_diffs = Vec::new();

    let left_workers = left_observability
        .worker_lineage
        .iter()
        .map(|worker| {
            (
                worker.worker_id.clone(),
                (
                    worker.status.clone(),
                    worker.run_id.clone(),
                    worker.run_path.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_workers = right_observability
        .worker_lineage
        .iter()
        .map(|worker| {
            (
                worker.worker_id.clone(),
                (
                    worker.status.clone(),
                    worker.run_id.clone(),
                    worker.run_path.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let worker_ids = left_workers
        .keys()
        .chain(right_workers.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for worker_id in worker_ids {
        match (left_workers.get(&worker_id), right_workers.get(&worker_id)) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "worker_lineage".to_string(),
                label: worker_id,
                details: vec!["worker missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "worker_lineage".to_string(),
                label: worker_id,
                details: vec!["worker missing from left run".to_string()],
            }),
            (Some(left_worker), Some(right_worker)) if left_worker != right_worker => {
                let mut details = Vec::new();
                if left_worker.0 != right_worker.0 {
                    details.push(format!("status: {} -> {}", left_worker.0, right_worker.0));
                }
                if left_worker.1 != right_worker.1 {
                    details.push(format!(
                        "run_id: {:?} -> {:?}",
                        left_worker.1, right_worker.1
                    ));
                }
                if left_worker.2 != right_worker.2 {
                    details.push(format!(
                        "run_path: {:?} -> {:?}",
                        left_worker.2, right_worker.2
                    ));
                }
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "worker_lineage".to_string(),
                    label: worker_id,
                    details,
                });
            }
            _ => {}
        }
    }

    let left_rounds = left_observability
        .planner_rounds
        .iter()
        .map(|round| (round.stage_id.clone(), round))
        .collect::<BTreeMap<_, _>>();
    let right_rounds = right_observability
        .planner_rounds
        .iter()
        .map(|round| (round.stage_id.clone(), round))
        .collect::<BTreeMap<_, _>>();
    let round_ids = left_rounds
        .keys()
        .chain(right_rounds.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for stage_id in round_ids {
        match (left_rounds.get(&stage_id), right_rounds.get(&stage_id)) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "planner_rounds".to_string(),
                label: stage_id,
                details: vec!["planner summary missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "planner_rounds".to_string(),
                label: stage_id,
                details: vec!["planner summary missing from left run".to_string()],
            }),
            (Some(left_round), Some(right_round)) => {
                let mut details = Vec::new();
                if left_round.iteration_count != right_round.iteration_count {
                    details.push(format!(
                        "iterations: {} -> {}",
                        left_round.iteration_count, right_round.iteration_count
                    ));
                }
                if left_round.tool_execution_count != right_round.tool_execution_count {
                    details.push(format!(
                        "tool_executions: {} -> {}",
                        left_round.tool_execution_count, right_round.tool_execution_count
                    ));
                }
                if left_round.native_text_tool_fallback_count
                    != right_round.native_text_tool_fallback_count
                {
                    details.push(format!(
                        "native_text_tool_fallbacks: {} -> {}",
                        left_round.native_text_tool_fallback_count,
                        right_round.native_text_tool_fallback_count
                    ));
                }
                if left_round.native_text_tool_fallback_rejection_count
                    != right_round.native_text_tool_fallback_rejection_count
                {
                    details.push(format!(
                        "native_text_tool_fallback_rejections: {} -> {}",
                        left_round.native_text_tool_fallback_rejection_count,
                        right_round.native_text_tool_fallback_rejection_count
                    ));
                }
                if left_round.empty_completion_retry_count
                    != right_round.empty_completion_retry_count
                {
                    details.push(format!(
                        "empty_completion_retries: {} -> {}",
                        left_round.empty_completion_retry_count,
                        right_round.empty_completion_retry_count
                    ));
                }
                if left_round.research_facts != right_round.research_facts {
                    details.push(format!(
                        "research_facts: {:?} -> {:?}",
                        left_round.research_facts, right_round.research_facts
                    ));
                }
                let left_deliverables = left_round
                    .task_ledger
                    .as_ref()
                    .map(|ledger| {
                        ledger
                            .deliverables
                            .iter()
                            .map(|item| format!("{}:{}", item.id, item.status))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let right_deliverables = right_round
                    .task_ledger
                    .as_ref()
                    .map(|ledger| {
                        ledger
                            .deliverables
                            .iter()
                            .map(|item| format!("{}:{}", item.id, item.status))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if left_deliverables != right_deliverables {
                    details.push(format!(
                        "deliverables: {:?} -> {:?}",
                        left_deliverables, right_deliverables
                    ));
                }
                if left_round.successful_tools != right_round.successful_tools {
                    details.push(format!(
                        "successful_tools: {:?} -> {:?}",
                        left_round.successful_tools, right_round.successful_tools
                    ));
                }
                if !details.is_empty() {
                    observability_diffs.push(RunObservabilityDiffRecord {
                        section: "planner_rounds".to_string(),
                        label: left_round.node_id.clone(),
                        details,
                    });
                }
            }
            _ => {}
        }
    }

    let left_pointers = left_observability
        .transcript_pointers
        .iter()
        .map(|pointer| {
            (
                pointer.id.clone(),
                (
                    pointer.available,
                    pointer.path.clone(),
                    pointer.location.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_pointers = right_observability
        .transcript_pointers
        .iter()
        .map(|pointer| {
            (
                pointer.id.clone(),
                (
                    pointer.available,
                    pointer.path.clone(),
                    pointer.location.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let pointer_ids = left_pointers
        .keys()
        .chain(right_pointers.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for pointer_id in pointer_ids {
        match (
            left_pointers.get(&pointer_id),
            right_pointers.get(&pointer_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "transcript_pointers".to_string(),
                label: pointer_id,
                details: vec!["pointer missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "transcript_pointers".to_string(),
                label: pointer_id,
                details: vec!["pointer missing from left run".to_string()],
            }),
            (Some(left_pointer), Some(right_pointer)) if left_pointer != right_pointer => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "transcript_pointers".to_string(),
                    label: pointer_id,
                    details: vec![format!(
                        "pointer: {:?} -> {:?}",
                        left_pointer, right_pointer
                    )],
                });
            }
            _ => {}
        }
    }

    let left_compactions = left_observability
        .compaction_events
        .iter()
        .map(|event| {
            (
                event.id.clone(),
                (
                    event.strategy.clone(),
                    event.archived_messages,
                    event.snapshot_asset_id.clone(),
                    event.available,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_compactions = right_observability
        .compaction_events
        .iter()
        .map(|event| {
            (
                event.id.clone(),
                (
                    event.strategy.clone(),
                    event.archived_messages,
                    event.snapshot_asset_id.clone(),
                    event.available,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let compaction_ids = left_compactions
        .keys()
        .chain(right_compactions.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for compaction_id in compaction_ids {
        match (
            left_compactions.get(&compaction_id),
            right_compactions.get(&compaction_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "compaction_events".to_string(),
                label: compaction_id,
                details: vec!["compaction event missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "compaction_events".to_string(),
                label: compaction_id,
                details: vec!["compaction event missing from left run".to_string()],
            }),
            (Some(left_event), Some(right_event)) if left_event != right_event => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "compaction_events".to_string(),
                    label: compaction_id,
                    details: vec![format!("event: {:?} -> {:?}", left_event, right_event)],
                });
            }
            _ => {}
        }
    }

    let left_daemons = left_observability
        .daemon_events
        .iter()
        .map(|event| {
            (
                (event.daemon_id.clone(), event.kind, event.timestamp.clone()),
                (
                    event.name.clone(),
                    event.persist_path.clone(),
                    event.payload_summary.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let right_daemons = right_observability
        .daemon_events
        .iter()
        .map(|event| {
            (
                (event.daemon_id.clone(), event.kind, event.timestamp.clone()),
                (
                    event.name.clone(),
                    event.persist_path.clone(),
                    event.payload_summary.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let daemon_keys = left_daemons
        .keys()
        .chain(right_daemons.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for daemon_key in daemon_keys {
        let label = format!("{}:{:?}:{}", daemon_key.0, daemon_key.1, daemon_key.2);
        match (
            left_daemons.get(&daemon_key),
            right_daemons.get(&daemon_key),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "daemon_events".to_string(),
                label,
                details: vec!["daemon event missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "daemon_events".to_string(),
                label,
                details: vec!["daemon event missing from left run".to_string()],
            }),
            (Some(left_event), Some(right_event)) if left_event != right_event => {
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "daemon_events".to_string(),
                    label,
                    details: vec![format!("event: {:?} -> {:?}", left_event, right_event)],
                });
            }
            _ => {}
        }
    }

    let left_verification = left_observability
        .verification_outcomes
        .iter()
        .map(|item| (item.stage_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let right_verification = right_observability
        .verification_outcomes
        .iter()
        .map(|item| (item.stage_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let verification_ids = left_verification
        .keys()
        .chain(right_verification.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for stage_id in verification_ids {
        match (
            left_verification.get(&stage_id),
            right_verification.get(&stage_id),
        ) {
            (Some(_), None) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "verification".to_string(),
                label: stage_id,
                details: vec!["verification missing from right run".to_string()],
            }),
            (None, Some(_)) => observability_diffs.push(RunObservabilityDiffRecord {
                section: "verification".to_string(),
                label: stage_id,
                details: vec!["verification missing from left run".to_string()],
            }),
            (Some(left_item), Some(right_item)) if left_item != right_item => {
                let mut details = Vec::new();
                if left_item.passed != right_item.passed {
                    details.push(format!(
                        "passed: {:?} -> {:?}",
                        left_item.passed, right_item.passed
                    ));
                }
                if left_item.summary != right_item.summary {
                    details.push(format!(
                        "summary: {:?} -> {:?}",
                        left_item.summary, right_item.summary
                    ));
                }
                observability_diffs.push(RunObservabilityDiffRecord {
                    section: "verification".to_string(),
                    label: left_item.node_id.clone(),
                    details,
                });
            }
            _ => {}
        }
    }

    let left_graph = (
        left_observability.action_graph_nodes.len(),
        left_observability.action_graph_edges.len(),
    );
    let right_graph = (
        right_observability.action_graph_nodes.len(),
        right_observability.action_graph_edges.len(),
    );
    if left_graph != right_graph {
        observability_diffs.push(RunObservabilityDiffRecord {
            section: "action_graph".to_string(),
            label: "shape".to_string(),
            details: vec![format!(
                "nodes/edges: {}/{} -> {}/{}",
                left_graph.0, left_graph.1, right_graph.0, right_graph.1
            )],
        });
    }

    let status_changed = left.status != right.status;
    let identical = !status_changed
        && stage_diffs.is_empty()
        && tool_diffs.is_empty()
        && observability_diffs.is_empty()
        && left.transitions.len() == right.transitions.len()
        && left.artifacts.len() == right.artifacts.len()
        && left.checkpoints.len() == right.checkpoints.len();

    RunDiffReport {
        left_run_id: left.id.clone(),
        right_run_id: right.id.clone(),
        identical,
        status_changed,
        left_status: left.status.clone(),
        right_status: right.status.clone(),
        stage_diffs,
        tool_diffs,
        observability_diffs,
        transition_count_delta: right.transitions.len() as isize - left.transitions.len() as isize,
        artifact_count_delta: right.artifacts.len() as isize - left.artifacts.len() as isize,
        checkpoint_count_delta: right.checkpoints.len() as isize - left.checkpoints.len() as isize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn minimal_run(status: &str) -> RunRecord {
        RunRecord {
            type_name: "workflow_run".to_string(),
            id: "run_1".to_string(),
            workflow_id: "workflow_1".to_string(),
            status: status.to_string(),
            usage: Some(LlmUsageRecord {
                total_duration_ms: 12,
                total_cost: 0.01,
                input_tokens: 3,
                output_tokens: 4,
                call_count: 1,
                models: vec!["mock".to_string()],
            }),
            replay_fixture: Some(ReplayFixture {
                type_name: "replay_fixture".to_string(),
                expected_status: "completed".to_string(),
                ..ReplayFixture::default()
            }),
            ..RunRecord::default()
        }
    }

    #[test]
    fn eval_pack_manifest_toml_runs_replay_case() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        fs::write(
            &run_path,
            serde_json::to_string(&minimal_run("completed")).unwrap(),
        )
        .unwrap();
        let pack_path = temp.path().join("harn.eval.toml");
        fs::write(
            &pack_path,
            r#"
version = 1
id = "connector-regressions"
name = "Connector regressions"

[[cases]]
id = "webhook"
name = "Webhook normalization"
run = "run.json"
rubrics = ["status"]

[[rubrics]]
id = "status"
kind = "deterministic"

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"
"#,
        )
        .unwrap();

        let manifest = load_eval_pack_manifest(&pack_path).unwrap();
        let report = evaluate_eval_pack_manifest(&manifest).unwrap();

        assert!(report.pass);
        assert_eq!(report.total, 1);
        assert_eq!(report.cases[0].label, "Webhook normalization");
    }

    #[test]
    fn eval_pack_warning_case_does_not_block() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        fs::write(
            &run_path,
            serde_json::to_string(&minimal_run("completed")).unwrap(),
        )
        .unwrap();
        let pack_path = temp.path().join("harn.eval.toml");
        fs::write(
            &pack_path,
            r#"
version = 1
id = "budgets"

[[cases]]
id = "latency-budget"
run = "run.json"
severity = "warning"

[cases.thresholds]
max-latency-ms = 1
"#,
        )
        .unwrap();

        let manifest = load_eval_pack_manifest(&pack_path).unwrap();
        let report = evaluate_eval_pack_manifest(&manifest).unwrap();

        assert!(report.pass);
        assert_eq!(report.warning_failed, 1);
        assert!(report.cases[0].warnings[0].contains("latency"));
    }

    #[test]
    fn eval_pack_manifest_runs_friction_context_pack_case() {
        let temp = tempfile::tempdir().unwrap();
        let events_path = temp.path().join("incident-friction.json");
        fs::write(
            &events_path,
            r#"
{
  "events": [
    {
      "kind": "repeated_query",
      "source": "incident-triage",
      "actor": "sre",
      "tool": "splunk",
      "provider": "splunk",
      "redacted_summary": "Checkout incidents need the same Splunk search",
      "recurrence_hints": ["checkout incident queries"],
      "estimated_time_ms": 300000,
      "metadata": {
        "query": "index=checkout service=api error",
        "capability": "splunk.search",
        "secret_ref": "SPLUNK_READ_TOKEN",
        "output_slot": "splunk_errors"
      }
    },
    {
      "kind": "repeated_query",
      "source": "incident-triage",
      "actor": "sre",
      "tool": "splunk",
      "provider": "splunk",
      "redacted_summary": "Checkout incident triage repeated the Splunk search",
      "recurrence_hints": ["checkout incident queries"],
      "estimated_time_ms": 240000,
      "metadata": {
        "query": "index=checkout service=api error",
        "capability": "splunk.search",
        "secret_ref": "SPLUNK_READ_TOKEN",
        "output_slot": "splunk_errors"
      }
    }
  ]
}
"#,
        )
        .unwrap();
        let pack_path = temp.path().join("harn.eval.toml");
        fs::write(
            &pack_path,
            r#"
version = 1
id = "team-learning"
name = "Team learning evals"

[[fixtures]]
id = "incident-friction"
kind = "friction-events"
path = "incident-friction.json"

[[cases]]
id = "incident-context-pack"
name = "Incident context pack suggestion"
friction_events = "incident-friction"
rubrics = ["context-pack"]

[[rubrics]]
id = "context-pack"
kind = "friction"

[[rubrics.assertions]]
kind = "context-pack-suggestion"
contains = "incident"
expected = { min_suggestions = 1, recommended_artifact = "context_pack", required_capability = "splunk.search", required_output_slot = "splunk_errors" }
"#,
        )
        .unwrap();

        let manifest = load_eval_pack_manifest(&pack_path).unwrap();
        let report = evaluate_eval_pack_manifest(&manifest).unwrap();

        assert!(report.pass);
        assert_eq!(report.total, 1);
        assert_eq!(report.cases[0].run_id, "friction_events");
        assert_eq!(report.cases[0].stage_count, 2);
    }
}
