//! Plain-data record types and action graph constants used across run records, replay fixtures,
//! eval reports, and diff utilities.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::{
    ArtifactRecord, CapabilityPolicy, HandoffArtifact, PersonaEvalLadderManifest,
    PersonaEvalLadderReport,
};
use crate::personas::{
    PersonaAssignmentStatus, PersonaBudgetStatus, PersonaHandoffInboxItem, PersonaQueuedWork,
    PersonaStatus, PersonaValueReceipt,
};

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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
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
    pub verification_status: String,
    pub verification_error: Option<String>,
    pub descriptor: Option<RunTranscriptArtifactDescriptor>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTranscriptArtifactDescriptor {
    pub schema_version: String,
    pub artifact_kind: String,
    pub run_id: String,
    pub session_id: Option<String>,
    pub path: String,
    pub relative_path: Option<String>,
    pub sha256: String,
    pub byte_len: u64,
    pub event_count: usize,
    pub first_event_type: Option<String>,
    pub first_event_id: Option<String>,
    pub last_event_type: Option<String>,
    pub last_event_id: Option<String>,
    pub complete: bool,
    pub terminal_status: Option<String>,
    pub effective_tool_format: Option<String>,
    pub tool_schema_hash: Option<String>,
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
    pub instruction_mode: String,
    pub instruction_source: Option<String>,
    pub compaction_policy: Option<serde_json::Value>,
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
    pub executor: Option<EvalPackCommandSpec>,
    pub trials: usize,
    pub split: Option<EvalPackSplit>,
    pub package: Option<EvalPackPackage>,
    pub defaults: EvalPackDefaults,
    pub fixtures: Vec<EvalPackFixtureRef>,
    pub rubrics: Vec<EvalPackRubric>,
    pub judge: Option<EvalPackJudgeConfig>,
    pub cases: Vec<EvalPackCase>,
    pub ladders: Vec<PersonaEvalLadderManifest>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackSplit {
    #[serde(flatten)]
    pub partitions: BTreeMap<String, Vec<String>>,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EvalPackCommandSpec {
    Shell(String),
    Argv(Vec<String>),
    Object(EvalPackCommandObject),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackCommandObject {
    pub command: Option<String>,
    #[serde(default, alias = "args")]
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    #[serde(
        default,
        alias = "timeout-seconds",
        alias = "timeoutSeconds",
        alias = "timeout_secs",
        alias = "timeout-secs",
        alias = "timeoutSecs"
    )]
    pub timeout_seconds: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackCase {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default, alias = "type")]
    pub kind: Option<String>,
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
    pub task: Option<String>,
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub executor: Option<EvalPackCommandSpec>,
    #[serde(
        default,
        alias = "verify",
        alias = "verify-command",
        alias = "verifyCommand"
    )]
    pub verify_command: Option<EvalPackCommandSpec>,
    #[serde(
        default,
        alias = "expected-output-paths",
        alias = "expectedOutputPaths"
    )]
    pub expected_output_paths: Vec<String>,
    #[serde(
        default,
        alias = "required-output-snippets",
        alias = "requiredOutputSnippets"
    )]
    pub required_output_snippets: Vec<String>,
    #[serde(default, alias = "tool-budget", alias = "toolBudget")]
    pub tool_budgets: BTreeMap<String, usize>,
    pub rubrics: Vec<String>,
    pub severity: Option<String>,
    pub trials: Option<usize>,
    #[serde(
        default,
        alias = "case-fingerprint",
        alias = "caseFingerprint",
        alias = "fingerprint"
    )]
    pub case_fingerprint: String,
    pub thresholds: EvalPackThresholds,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackReport {
    pub pack_id: String,
    pub harness_config_fingerprint: String,
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub blocking_failed: usize,
    pub warning_failed: usize,
    pub informational_failed: usize,
    pub trial_count: usize,
    pub run_state: EvalPackRunState,
    pub split: Option<EvalPackSplitValidationReport>,
    pub stats: EvalPackStatsReport,
    pub stats_rows: Vec<EvalPackStatsRow>,
    pub cases: Vec<EvalPackCaseReport>,
    pub ladders: Vec<PersonaEvalLadderReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackCaseReport {
    pub id: String,
    pub label: String,
    pub severity: String,
    pub split: Option<String>,
    pub case_fingerprint: String,
    pub harness_config_fingerprint: String,
    pub pass: bool,
    pub blocking: bool,
    pub run_id: String,
    pub workflow_id: String,
    pub source_path: Option<String>,
    pub stage_count: usize,
    pub trial_count: usize,
    pub total_stage_count: usize,
    pub reliability: EvalPackReliabilityReport,
    pub stats_row: EvalPackStatsRow,
    pub trials: Vec<EvalPackTrialReport>,
    pub failures: Vec<String>,
    pub warnings: Vec<String>,
    pub informational: Vec<String>,
    pub comparison: Option<RunDiffReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackTrialReport {
    pub trial: usize,
    pub verification: String,
    #[serde(default, alias = "verificationExitCode")]
    pub verification_exit_code: Option<i64>,
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
    pub timed_out: bool,
    pub wall_time_seconds: f64,
    pub cost_usd: f64,
    #[serde(default, alias = "producedPaths")]
    pub produced_paths: Vec<String>,
    #[serde(default, alias = "toolCallSummary", alias = "tool_summary")]
    pub tool_call_summary: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackReliabilityReport {
    pub status: String,
    pub trials: usize,
    pub passes: usize,
    pub fails: usize,
    pub skips: usize,
    pub timeouts: usize,
    pub decided: usize,
    pub pass_rate: f64,
    pub majority: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackStatsRow {
    pub name: String,
    pub case_name: String,
    pub case_fingerprint: String,
    pub harness_config_fingerprint: String,
    pub group: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub split: Option<String>,
    pub trials: usize,
    pub passes: usize,
    pub fails: usize,
    pub skips: usize,
    pub timeouts: usize,
    pub pass_rate: f64,
    pub status: String,
    pub majority: Option<String>,
    pub wall_time_seconds: f64,
    pub cost_usd: f64,
    pub mean_wall_time_seconds: f64,
    pub stdev_wall_time_seconds: f64,
    pub total_cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackStatsReport {
    pub macro_pass_at_1: f64,
    pub reliability: EvalPackReliabilityBreakdown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackRunState {
    pub schema: String,
    pub suite: String,
    pub model: String,
    pub commit: String,
    pub branch: Option<String>,
    pub requested_cells: usize,
    pub completed_cells: usize,
    pub skipped_cells: usize,
    pub executed_cells: usize,
    pub remaining_cells: usize,
    pub ledger_rows_inserted: usize,
    pub ledger_rows_duplicate: usize,
    pub fingerprint_refusals: usize,
    pub all_skipped: bool,
    pub heartbeat_event_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalPackReliabilityBreakdown {
    pub all_pass_cases: usize,
    pub flaky_cases: usize,
    pub all_fail_cases: usize,
    pub no_decision_cases: usize,
    pub total_cases: usize,
    pub all_pass_fraction: f64,
    pub flaky_fraction: f64,
    pub all_fail_fraction: f64,
    pub no_decision_fraction: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalPackSplitValidationReport {
    pub valid: bool,
    pub partitions: BTreeMap<String, Vec<String>>,
    pub case_count: usize,
    pub covered_count: usize,
    pub duplicate_case_ids: Vec<String>,
    pub duplicate_partition_cases: Vec<String>,
    pub overlap_cases: Vec<String>,
    pub unknown_cases: Vec<String>,
    pub missing_cases: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalLedgerProvenance {
    pub commit: String,
    pub branch: Option<String>,
    pub ts: String,
    pub harn_version: String,
    pub host: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalLedgerRow {
    pub event_id: Option<u64>,
    pub schema: String,
    pub suite: String,
    pub model: String,
    pub split: Option<String>,
    pub commit: String,
    pub case_name: String,
    pub name: String,
    pub case_fingerprint: String,
    pub harness_config_fingerprint: String,
    pub trial: usize,
    pub trials: usize,
    pub passes: usize,
    pub fails: usize,
    pub skips: usize,
    pub timeouts: usize,
    pub pass_rate: f64,
    pub status: String,
    pub majority: Option<String>,
    pub verification: String,
    pub skipped: bool,
    pub wall_time_seconds: f64,
    pub cost_usd: f64,
    pub mean_wall_time_seconds: f64,
    pub stdev_wall_time_seconds: f64,
    pub total_cost_usd: f64,
    pub run_id: String,
    pub workflow_id: String,
    pub source_path: Option<String>,
    pub trial_report: Option<EvalPackTrialReport>,
    pub provenance: EvalLedgerProvenance,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalLedgerAppendReport {
    pub rows: Vec<EvalLedgerRow>,
    pub appended: usize,
    pub inserted: usize,
    pub duplicates: usize,
    pub all_skipped: bool,
    pub event_ids: Vec<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalLedgerReadReport {
    pub rows: Vec<EvalLedgerRow>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalLedgerFingerprintMismatch {
    pub case_name: String,
    pub split: Option<String>,
    pub commit: String,
    pub trial: usize,
    pub case_fingerprint: String,
    pub harness_config_fingerprint: String,
    pub expected_case_fingerprint: String,
    pub expected_harness_config_fingerprint: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EvalLedgerPriorCommitReport {
    pub commit: Option<String>,
    pub model: String,
    pub split: Option<String>,
    pub rows: Vec<EvalLedgerRow>,
    pub fingerprint_mismatches: Vec<EvalLedgerFingerprintMismatch>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalLedgerResumeCell {
    pub case_name: String,
    pub split: Option<String>,
    pub trial: usize,
    pub status: String,
    pub reason: String,
    pub event_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalLedgerResumePlan {
    pub schema: String,
    pub suite: String,
    pub model: String,
    pub commit: String,
    pub harness_config_fingerprint: String,
    pub requested_cells: usize,
    pub completed_cells: usize,
    pub skipped_cells: usize,
    pub remaining_cells: usize,
    pub all_skipped: bool,
    pub fingerprint_refusals: Vec<EvalLedgerFingerprintMismatch>,
    pub cells: Vec<EvalLedgerResumeCell>,
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
pub struct RunPersonaRuntimeRecord {
    pub name: String,
    pub role: String,
    pub template_ref: Option<String>,
    pub state: String,
    pub entry_workflow: String,
    pub current_assignment: Option<PersonaAssignmentStatus>,
    pub queued_work: Vec<PersonaQueuedWork>,
    pub handoff_inbox: Vec<PersonaHandoffInboxItem>,
    pub budget: PersonaBudgetStatus,
    pub value_receipts: Vec<PersonaValueReceipt>,
    pub last_run: Option<String>,
    pub last_error: Option<String>,
}

impl From<&PersonaStatus> for RunPersonaRuntimeRecord {
    fn from(status: &PersonaStatus) -> Self {
        Self {
            name: status.name.clone(),
            role: status.role.clone(),
            template_ref: status.template_ref.clone(),
            state: status.state.as_str().to_string(),
            entry_workflow: status.entry_workflow.clone(),
            current_assignment: status.current_assignment.clone(),
            queued_work: status.queued_work.clone(),
            handoff_inbox: status.handoff_inbox.clone(),
            budget: status.budget.clone(),
            value_receipts: status.value_receipts.clone(),
            last_run: status.last_run.clone(),
            last_error: status.last_error.clone(),
        }
    }
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
    pub persona_runtime: Vec<RunPersonaRuntimeRecord>,
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

// Not `Eq`: `cost_usd: Option<f64>` carries a float, so the record is only
// `PartialEq`. Nothing keys a set/map on trace spans, so this is inert.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunTraceSpanRecord {
    pub trace_id: String,
    pub span_id: u64,
    #[serde(rename = "parent_span_id", alias = "parent_id")]
    pub parent_id: Option<u64>,
    pub kind: String,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Time to first streamed response token for an LLM span. This is a
    /// first-class projection of the collector's `first_token_ms` metadata;
    /// `None` for non-LLM and non-streaming spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub links: Vec<crate::tracing::SpanLink>,
    /// First-class per-call cost projection for `llm_call` spans, in USD.
    /// Mirrors the `cost_usd` metadata key so downstream viewers can build
    /// cost flame graphs without parsing the metadata map. `None` for
    /// non-LLM spans and for `llm_call` spans whose (provider, model) pair
    /// has no catalog pricing. Defaults to `None` when absent, so records
    /// persisted before this field existed still load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
    pub approval_policy: Option<super::super::ToolApprovalPolicy>,
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
                serde_json::from_value::<super::super::ToolApprovalPolicy>(value.clone()).ok()
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunExecutionRecord {
    pub cwd: Option<String>,
    pub project_root: Option<String>,
    pub source_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub adapter: Option<String>,
    pub repo_path: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub cleanup: Option<String>,
    /// Non-secret receipts for the session-scoped capability grants this run
    /// launched under (empty for a hermetic run, which is the default). Each
    /// receipt records the grant name, its source kind, and whether it was
    /// exposed to the process environment — never the credential value. See
    /// [`crate::security::GrantReceipt`]. `#[serde(default)]` on the struct
    /// loads pre-grants records with an empty vec.
    pub grants: Vec<crate::security::GrantReceipt>,
}

#[cfg(test)]
mod trace_span_record_tests {
    use super::*;

    #[test]
    fn trace_span_record_round_trips_with_cost_and_token_metadata() {
        let span = RunTraceSpanRecord {
            trace_id: "trace_1".to_string(),
            span_id: 7,
            parent_id: Some(3),
            kind: "llm_call".to_string(),
            name: "llm_call".to_string(),
            start_ms: 120,
            duration_ms: 900,
            ttft_ms: Some(125),
            metadata: BTreeMap::from([
                ("model".to_string(), serde_json::json!("claude-sonnet-4")),
                ("provider".to_string(), serde_json::json!("anthropic")),
                ("input_tokens".to_string(), serde_json::json!(1200)),
                ("output_tokens".to_string(), serde_json::json!(340)),
                ("cache_read_tokens".to_string(), serde_json::json!(800)),
                ("cache_write_tokens".to_string(), serde_json::json!(64)),
                ("cost_usd".to_string(), serde_json::json!(0.0123)),
            ]),
            links: Vec::new(),
            cost_usd: Some(0.0123),
        };
        let encoded = serde_json::to_string(&span).unwrap();
        let decoded: RunTraceSpanRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, span);
        let json: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(json["parent_span_id"], serde_json::json!(3));
        assert_eq!(json["ttft_ms"], serde_json::json!(125));
        assert!(json.get("parent_id").is_none());
        assert_eq!(decoded.cost_usd, Some(0.0123));
        assert_eq!(
            decoded.metadata["cache_read_tokens"],
            serde_json::json!(800)
        );
    }

    #[test]
    fn trace_span_record_loads_legacy_json_without_cost_field() {
        // A record persisted before `cost_usd` existed. `#[serde(default)]`
        // on the struct must fill the missing field with `None` rather than
        // failing the whole RunRecord load.
        let legacy = serde_json::json!({
            "trace_id": "trace_legacy",
            "span_id": 2,
            "parent_id": 11,
            "kind": "llm_call",
            "name": "llm_call",
            "start_ms": 0,
            "duration_ms": 500,
            "metadata": { "model": "gpt-4o-mini", "input_tokens": 10 },
            "links": []
        });
        let decoded: RunTraceSpanRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.parent_id, Some(11));
        assert_eq!(decoded.cost_usd, None);
        assert_eq!(decoded.kind, "llm_call");
        assert_eq!(decoded.metadata["model"], serde_json::json!("gpt-4o-mini"));
    }

    #[test]
    fn trace_span_record_omits_cost_field_when_absent() {
        // Non-LLM spans leave `cost_usd` unset; the field is skipped on the
        // wire so the persisted shape stays byte-identical to pre-change
        // records for spans that carry no cost.
        let span = RunTraceSpanRecord {
            trace_id: "trace_1".to_string(),
            span_id: 1,
            kind: "tool_call".to_string(),
            name: "read".to_string(),
            ..RunTraceSpanRecord::default()
        };
        let encoded = serde_json::to_value(&span).unwrap();
        assert!(encoded.get("cost_usd").is_none());
    }

    #[test]
    fn legacy_run_record_without_new_span_kinds_still_loads() {
        // A RunRecord whose trace carries the new marker span kinds must
        // deserialize (kinds are free-form strings on the record), and an
        // older record without them is unaffected.
        let json = serde_json::json!({
            "trace_spans": [
                { "trace_id": "t", "span_id": 1, "kind": "model_route", "name": "model_route",
                  "metadata": { "from_model": "a", "to_model": "b", "reason": "escalation" } },
                { "trace_id": "t", "span_id": 2, "kind": "tool_mount", "name": "tool_mount",
                  "metadata": { "source": "mcp", "tool_count": 3 } }
            ]
        });
        let decoded: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.trace_spans.len(), 2);
        assert_eq!(decoded.trace_spans[0].kind, "model_route");
        assert_eq!(
            decoded.trace_spans[1].metadata["source"],
            serde_json::json!("mcp")
        );
    }
}
