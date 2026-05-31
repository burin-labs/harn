//! Data types for the crystallization pipeline (traces, candidates, reports, options).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::super::{ReplayAllowlistRule, ReplayOracleReport, ReplayTraceRun};

pub(super) const TRACE_SCHEMA_VERSION: u32 = 1;
pub(super) const DEFAULT_MIN_EXAMPLES: usize = 2;
pub(super) const DEFAULT_PROMOTION_MIN_CONFIDENCE: f64 = 0.80;

/// Stable schema marker for `candidate.json` inside a crystallization
/// bundle. Cloud importers and other downstream consumers should refuse
/// bundles whose `schema` field is anything else.
pub const BUNDLE_SCHEMA: &str = "harn.crystallization.candidate.bundle";
/// Versioned schema number for the bundle manifest. Cloud importers and
/// other consumers should refuse bundles whose `schema_version` is newer
/// than the highest version they understand.
pub const BUNDLE_SCHEMA_VERSION: u32 = 1;
/// Conventional file names inside a crystallization bundle directory.
pub const BUNDLE_MANIFEST_FILE: &str = "candidate.json";
pub const BUNDLE_REPORT_FILE: &str = "report.json";
pub const BUNDLE_WORKFLOW_FILE: &str = "workflow.harn";
pub const BUNDLE_EVAL_PACK_FILE: &str = "harn.eval.toml";
pub const BUNDLE_FIXTURES_DIR: &str = "fixtures";
pub const BUNDLE_SKILL_DIR: &str = "skill";
pub const BUNDLE_SKILL_FILE: &str = "SKILL.md";
pub const BUNDLE_SKILL_GATE_FILE: &str = "gate.json";
pub const SKILL_CANDIDATE_SCHEMA: &str = "harn.crystallization.skill_candidate";
pub const SKILL_CANDIDATE_SCHEMA_VERSION: u32 = 1;
pub const SKILL_GATE_RECEIPT_SCHEMA: &str = "harn.skill_induction.replay_gate.v1";

/// Default rollout policy applied when a bundle is emitted without one
/// explicitly configured. Hosted promotion surfaces can override it.
pub(super) const DEFAULT_ROLLOUT_POLICY: &str = "shadow_then_canary";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationTrace {
    pub version: u32,
    pub id: String,
    pub source: Option<String>,
    pub source_hash: Option<String>,
    pub workflow_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub flow: Option<CrystallizationFlowRef>,
    pub actions: Vec<CrystallizationAction>,
    pub replay_run: Option<ReplayTraceRun>,
    pub replay_allowlist: Vec<ReplayAllowlistRule>,
    pub usage: CrystallizationUsage,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CrystallizationFlowRef {
    pub trace_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub transcript_ref: Option<String>,
    pub atom_ids: Vec<String>,
    pub slice_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationAction {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub timestamp: Option<String>,
    pub inputs: JsonValue,
    pub output: Option<JsonValue>,
    pub observed_output: Option<JsonValue>,
    pub parameters: BTreeMap<String, JsonValue>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval: Option<CrystallizationApproval>,
    pub cost: CrystallizationCost,
    pub duration_ms: Option<i64>,
    pub deterministic: Option<bool>,
    pub fuzzy: Option<bool>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CrystallizationSideEffect {
    pub kind: String,
    pub target: String,
    pub capability: Option<String>,
    pub mutation: Option<String>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CrystallizationApproval {
    pub prompt: Option<String>,
    pub approver: Option<String>,
    pub required: bool,
    pub boundary: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationCost {
    pub model: Option<String>,
    pub model_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_cost_usd: f64,
    pub wall_ms: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationUsage {
    pub model_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_cost_usd: f64,
    pub wall_ms: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    #[default]
    Deterministic,
    Fuzzy,
}

pub(super) type SequenceExample = (usize, usize);
pub(super) type RepeatedSequence = (Vec<String>, Vec<SequenceExample>);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowCandidateParameter {
    pub name: String,
    pub source_paths: Vec<String>,
    pub examples: Vec<String>,
    pub required: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowCandidateStep {
    pub index: usize,
    pub kind: String,
    pub name: String,
    pub segment: SegmentKind,
    pub parameter_refs: Vec<String>,
    pub constants: BTreeMap<String, JsonValue>,
    pub preconditions: Vec<String>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval: Option<CrystallizationApproval>,
    pub expected_output: Option<JsonValue>,
    pub review_notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowCandidateExample {
    pub trace_id: String,
    pub source_hash: String,
    pub start_index: usize,
    pub action_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromotionMetadata {
    pub source_trace_hashes: Vec<String>,
    pub author: Option<String>,
    pub approver: Option<String>,
    pub created_at: String,
    pub version: String,
    pub package_name: String,
    pub capability_set: Vec<String>,
    pub secrets_required: Vec<String>,
    pub rollback_target: Option<String>,
    pub eval_pack_link: Option<String>,
    pub sample_count: usize,
    pub confidence: f64,
    pub shadow_success_count: usize,
    pub shadow_failure_count: usize,
    pub divergence_history: Vec<PromotionDivergenceRecord>,
    pub approval_history: Vec<PromotionApprovalRecord>,
    pub criteria: PromotionCriteria,
    pub estimated_time_token_savings: SavingsEstimate,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromotionCriteria {
    pub min_examples: usize,
    pub min_confidence: f64,
    pub requires_shadow_pass: bool,
    pub requires_no_rejections: bool,
    pub requires_human_approval: bool,
    pub approval_reason: Option<String>,
    pub status: PromotionStatus,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromotionStatus {
    #[default]
    Blocked,
    NeedsApproval,
    Ready,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PromotionDivergenceRecord {
    pub trace_id: String,
    pub path: Option<String>,
    pub message: String,
    pub left: Option<JsonValue>,
    pub right: Option<JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PromotionApprovalRecord {
    pub actor: String,
    pub decision: String,
    pub recorded_at: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SavingsEstimate {
    pub model_calls_avoided: i64,
    pub input_tokens_avoided: i64,
    pub output_tokens_avoided: i64,
    pub estimated_cost_usd_avoided: f64,
    pub wall_ms_avoided: i64,
    pub cpu_runtime_cost_usd: f64,
    pub remaining_model_calls: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShadowTraceResult {
    pub trace_id: String,
    pub source_hash: String,
    pub pass: bool,
    pub details: Vec<String>,
    pub compared_receipts: usize,
    pub replay_oracle: Option<ReplayOracleReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShadowRunReport {
    pub pass: bool,
    pub compared_traces: usize,
    pub failures: Vec<String>,
    pub traces: Vec<ShadowTraceResult>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowClusterKey {
    pub goal: Option<String>,
    pub tool_sequence: Vec<String>,
    pub touched_artifact_types: Vec<String>,
    pub success_criteria: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowCandidate {
    pub id: String,
    pub name: String,
    pub confidence: f64,
    pub cluster_key: WorkflowClusterKey,
    pub sequence_signature: Vec<String>,
    pub parameters: Vec<WorkflowCandidateParameter>,
    pub steps: Vec<WorkflowCandidateStep>,
    pub examples: Vec<WorkflowCandidateExample>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval_points: Vec<CrystallizationApproval>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub expected_outputs: Vec<JsonValue>,
    pub expected_receipts: Vec<JsonValue>,
    pub warnings: Vec<String>,
    pub rejection_reasons: Vec<String>,
    pub promotion: PromotionMetadata,
    pub savings: SavingsEstimate,
    pub shadow: ShadowRunReport,
}

impl WorkflowCandidate {
    pub fn is_safe_to_propose(&self) -> bool {
        self.rejection_reasons.is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillCandidateEvidenceRef {
    pub trace_id: String,
    pub source_hash: String,
    pub source_url: Option<String>,
    pub action_ids: Vec<String>,
    pub role: SkillCandidateEvidenceRole,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillCandidateEvidenceRole {
    #[default]
    Source,
    HeldOut,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillInductionGateReceipt {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: u32,
    pub skill_candidate_id: String,
    pub workflow_candidate_id: String,
    pub accepted: bool,
    pub decision: String,
    pub original_trace_count: usize,
    pub heldout_trace_count: usize,
    pub compared_trace_count: usize,
    pub failures: Vec<String>,
    pub replay_trace_ids: Vec<String>,
    pub heldout_trace_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillInductionReplayGate {
    pub original_replay_pass: bool,
    pub heldout_replay_pass: bool,
    pub original_trace_count: usize,
    pub heldout_trace_count: usize,
    pub compared_trace_count: usize,
    pub failures: Vec<String>,
    pub receipt: SkillInductionGateReceipt,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillCandidateArtifact {
    pub schema: String,
    pub schema_version: u32,
    pub id: String,
    pub workflow_candidate_id: String,
    pub name: String,
    pub short: String,
    pub description: String,
    pub when_to_use: String,
    pub allowed_tools: Vec<String>,
    pub paths: Vec<String>,
    pub source_trace_hashes: Vec<String>,
    pub evidence_refs: Vec<SkillCandidateEvidenceRef>,
    pub replay_gate: SkillInductionReplayGate,
    pub skill_markdown: String,
    pub warnings: Vec<String>,
    pub rejection_reasons: Vec<String>,
}

impl SkillCandidateArtifact {
    pub fn is_safe_to_propose(&self) -> bool {
        self.rejection_reasons.is_empty() && self.replay_gate.receipt.accepted
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationReport {
    pub version: u32,
    pub generated_at: String,
    pub source_trace_count: usize,
    pub excluded_trace_count: usize,
    pub selected_candidate_id: Option<String>,
    pub candidates: Vec<WorkflowCandidate>,
    pub rejected_candidates: Vec<WorkflowCandidate>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skill_candidates: Vec<SkillCandidateArtifact>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected_skill_candidates: Vec<SkillCandidateArtifact>,
    pub warnings: Vec<String>,
    pub input_format: CrystallizationInputFormat,
    pub harn_code_path: Option<String>,
    pub eval_pack_path: Option<String>,
    /// Optional plain-language explanation of which steps are safe to
    /// automate vs. which still require human/agent review. Populated by
    /// the release-fixture ingest path so reviewers can inspect a
    /// candidate without re-deriving the deterministic/agentic split.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_summary: Option<SegmentSummary>,
    /// Optional summary of how shell/tool failures were represented and
    /// whether failure context was fed back into a model. Populated by
    /// the release-fixture ingest path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_summary: Option<RecoveryFeedbackSummary>,
}

/// Plain-language summary of the deterministic/agentic split for a
/// candidate. Designed to be human-readable in `report.json` without a
/// reviewer needing to walk the step list manually.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SegmentSummary {
    pub deterministic_count: usize,
    pub agentic_count: usize,
    pub safe_to_automate: Vec<String>,
    pub requires_human_review: Vec<String>,
    pub plain_language: String,
}

/// Summary of how shell/tool failures were represented in the source
/// trace and whether the failure context was fed back into the model.
/// Lets reviewers see at a glance whether recovery was advisory only or
/// whether the workflow attempted to repair itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RecoveryFeedbackSummary {
    pub shell_failures_seen: usize,
    pub recovery_advice_runs: usize,
    /// `true` when at least one recovery action observed by the source
    /// trace fed the failing-step context into a model loop (vs. just
    /// recording the failure for human review).
    pub failures_fed_into_agent: bool,
    pub failed_steps: Vec<String>,
    /// Plain-language explanation of how recovery was represented.
    pub representation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CrystallizationInputFormat {
    pub name: String,
    pub version: u32,
    pub required_fields: Vec<String>,
    pub preserved_fields: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CrystallizeOptions {
    pub min_examples: usize,
    pub shadow_traces: Vec<CrystallizationTrace>,
    pub promotion_min_confidence: f64,
    pub workflow_name: Option<String>,
    pub package_name: Option<String>,
    pub author: Option<String>,
    pub approver: Option<String>,
    pub eval_pack_link: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CrystallizationArtifacts {
    pub report: CrystallizationReport,
    pub harn_code: String,
    pub eval_pack_toml: String,
}
