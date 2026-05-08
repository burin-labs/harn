//! Workflow crystallization primitives.
//!
//! This module keeps the first mining pass deliberately conservative: it
//! accepts ordered trace actions, finds a repeated contiguous action sequence,
//! extracts scalar parameters from fields that vary across examples, rejects
//! divergent side effects, and emits a reviewable Harn skeleton plus shadow
//! comparison metadata. Hosted surfaces can layer richer mining on top of this
//! stable IR without changing the CLI contract.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use super::{
    new_id, now_rfc3339, run_replay_oracle_trace, ReplayAllowlistRule, ReplayExpectation,
    ReplayFixture, ReplayOracleReport, ReplayOracleTrace, ReplayTraceRun, RunRecord,
};
use crate::value::VmError;

const TRACE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MIN_EXAMPLES: usize = 2;
const DEFAULT_PROMOTION_MIN_CONFIDENCE: f64 = 0.80;

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

/// Default rollout policy applied when a bundle is emitted without one
/// explicitly configured. Hosted promotion surfaces can override it.
const DEFAULT_ROLLOUT_POLICY: &str = "shadow_then_canary";

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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
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

type SequenceExample = (usize, usize);
type RepeatedSequence = (Vec<String>, Vec<SequenceExample>);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowCandidateParameter {
    pub name: String,
    pub source_paths: Vec<String>,
    pub examples: Vec<String>,
    pub required: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShadowTraceResult {
    pub trace_id: String,
    pub source_hash: String,
    pub pass: bool,
    pub details: Vec<String>,
    pub compared_receipts: usize,
    pub replay_oracle: Option<ReplayOracleReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
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

pub fn crystallize_traces(
    traces: Vec<CrystallizationTrace>,
    options: CrystallizeOptions,
) -> Result<CrystallizationArtifacts, VmError> {
    let min_examples = options.min_examples.max(DEFAULT_MIN_EXAMPLES);
    if traces.len() < min_examples {
        return Err(VmError::Runtime(format!(
            "crystallize requires at least {min_examples} traces, got {}",
            traces.len()
        )));
    }

    let normalized_all = traces.into_iter().map(normalize_trace).collect::<Vec<_>>();
    let (normalized, excluded_mining) = partition_mineable_traces(normalized_all);
    if normalized.len() < min_examples {
        return Err(VmError::Runtime(format!(
            "crystallize requires at least {min_examples} eligible traces, got {} (excluded {})",
            normalized.len(),
            excluded_mining.len()
        )));
    }
    let (shadow_traces, excluded_shadow) = partition_mineable_traces(
        options
            .shadow_traces
            .iter()
            .cloned()
            .map(normalize_trace)
            .collect(),
    );
    let mut shadow_pool = normalized.clone();
    shadow_pool.extend(shadow_traces);

    let mut candidates = mine_candidates(&normalized, min_examples, &options);
    let mut rejected_candidates = Vec::new();
    for candidate in &mut candidates {
        candidate.shadow = shadow_candidate(candidate, &shadow_pool);
        if !candidate.shadow.pass {
            candidate
                .rejection_reasons
                .extend(candidate.shadow.failures.clone());
        }
        refresh_promotion_metadata(candidate, min_examples, &options);
    }

    let mut accepted = Vec::new();
    for candidate in candidates {
        if candidate.is_safe_to_propose() {
            accepted.push(candidate);
        } else {
            rejected_candidates.push(candidate);
        }
    }
    accepted.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.steps.len().cmp(&left.steps.len()))
    });

    let selected = accepted.first();
    let harn_code = selected
        .map(generate_harn_code)
        .unwrap_or_else(|| rejected_workflow_stub(&rejected_candidates));
    let eval_pack_toml = selected.map(generate_eval_pack).unwrap_or_default();

    let report = CrystallizationReport {
        version: 1,
        generated_at: now_rfc3339(),
        source_trace_count: normalized.len(),
        excluded_trace_count: excluded_mining.len() + excluded_shadow.len(),
        selected_candidate_id: selected.map(|candidate| candidate.id.clone()),
        candidates: accepted,
        rejected_candidates,
        warnings: Vec::new(),
        input_format: CrystallizationInputFormat {
            name: "harn.crystallization.trace".to_string(),
            version: TRACE_SCHEMA_VERSION,
            required_fields: vec!["id".to_string(), "actions".to_string()],
            preserved_fields: vec![
                "ordered actions".to_string(),
                "tool calls".to_string(),
                "model calls".to_string(),
                "human approvals".to_string(),
                "file mutations".to_string(),
                "external API calls".to_string(),
                "observed outputs".to_string(),
                "costs".to_string(),
                "timestamps".to_string(),
                "source hashes".to_string(),
                "Flow provenance references".to_string(),
            ],
        },
        harn_code_path: None,
        eval_pack_path: None,
        segment_summary: None,
        recovery_summary: None,
    };

    Ok(CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    })
}

/// Synthesize a single-trace crystallization candidate without going
/// through repeated-sequence mining. Used by the release-fixture ingest
/// path where the trace is the workflow: there is exactly one example
/// and the deterministic/agentic split comes from the source events
/// directly. The resulting [`CrystallizationArtifacts`] is bundle-ready
/// (`build_crystallization_bundle`) and validates with the existing
/// validator.
///
/// `extra_parameters` and `extra_metadata` let callers seed parameters
/// (e.g., release identity fields like `current_version` /
/// `next_version`) and metadata (segment / recovery summaries) that the
/// trace alone does not surface.
pub fn synthesize_candidate_from_trace(
    trace: CrystallizationTrace,
    options: CrystallizeOptions,
    extra_parameters: Vec<WorkflowCandidateParameter>,
    segment_summary: Option<SegmentSummary>,
    recovery_summary: Option<RecoveryFeedbackSummary>,
) -> Result<CrystallizationArtifacts, VmError> {
    if trace.actions.is_empty() {
        return Err(VmError::Runtime(
            "synthesize_candidate_from_trace requires a trace with at least one action".to_string(),
        ));
    }
    let normalized = normalize_trace(trace);
    let mut candidate = build_single_trace_candidate(&normalized, &options, extra_parameters);
    candidate.shadow = shadow_candidate(&candidate, std::slice::from_ref(&normalized));
    if !candidate.shadow.pass {
        candidate
            .rejection_reasons
            .extend(candidate.shadow.failures.clone());
    }
    refresh_promotion_metadata(&mut candidate, 1, &options);
    let (accepted, rejected) = if candidate.is_safe_to_propose() {
        (vec![candidate], Vec::new())
    } else {
        (Vec::new(), vec![candidate])
    };

    let selected = accepted.first();
    let harn_code = selected
        .map(generate_harn_code)
        .unwrap_or_else(|| rejected_workflow_stub(&rejected));
    let eval_pack_toml = selected.map(generate_eval_pack).unwrap_or_default();
    let selected_id = selected.map(|candidate| candidate.id.clone());

    let report = CrystallizationReport {
        version: 1,
        generated_at: now_rfc3339(),
        source_trace_count: 1,
        excluded_trace_count: 0,
        selected_candidate_id: selected_id,
        candidates: accepted,
        rejected_candidates: rejected,
        warnings: Vec::new(),
        input_format: CrystallizationInputFormat {
            name: "harn.crystallization.trace".to_string(),
            version: TRACE_SCHEMA_VERSION,
            required_fields: vec!["id".to_string(), "actions".to_string()],
            preserved_fields: vec![
                "ordered actions".to_string(),
                "deterministic events".to_string(),
                "agentic events".to_string(),
                "tool/shell observations".to_string(),
                "recovery advice".to_string(),
                "release metadata".to_string(),
                "source hashes".to_string(),
            ],
        },
        harn_code_path: None,
        eval_pack_path: None,
        segment_summary,
        recovery_summary,
    };

    Ok(CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    })
}

/// Build a single-example candidate directly from a normalized trace.
/// This is the single-trace analog of `mine_candidates`: every action
/// becomes a step, every action's `fuzzy`/`model_call` flag drives the
/// segment kind, and parameters are taken from each action's
/// `parameters` map (plus any caller-supplied `extra_parameters`).
fn build_single_trace_candidate(
    trace: &CrystallizationTrace,
    options: &CrystallizeOptions,
    extra_parameters: Vec<WorkflowCandidateParameter>,
) -> WorkflowCandidate {
    let mut steps = Vec::with_capacity(trace.actions.len());
    let mut parameter_values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut parameter_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut warnings = Vec::new();
    let sequence: Vec<String> = trace.actions.iter().map(action_signature).collect();

    for (idx, action) in trace.actions.iter().enumerate() {
        let mut parameter_refs = BTreeSet::new();
        for (name, value) in &action.parameters {
            if is_scalar(value) {
                let ident = sanitize_identifier(name);
                parameter_values
                    .entry(ident.clone())
                    .or_default()
                    .insert(json_scalar_string(value));
                parameter_paths
                    .entry(ident.clone())
                    .or_default()
                    .insert(format!("steps[{idx}].parameters.{name}"));
                parameter_refs.insert(ident);
            }
        }

        let fuzzy = action.fuzzy.unwrap_or(false) || action.kind == "model_call";
        if fuzzy {
            warnings.push(format!(
                "step {} '{}' remains fuzzy and requires review/LLM handling",
                idx + 1,
                action.name
            ));
        }

        steps.push(WorkflowCandidateStep {
            index: idx + 1,
            kind: action.kind.clone(),
            name: action.name.clone(),
            segment: if fuzzy {
                SegmentKind::Fuzzy
            } else {
                SegmentKind::Deterministic
            },
            parameter_refs: parameter_refs.into_iter().collect(),
            constants: constants_for_action(action),
            preconditions: preconditions_for_action(action),
            side_effects: action.side_effects.clone(),
            capabilities: sorted_strings(action.capabilities.iter().cloned()),
            required_secrets: sorted_strings(action.required_secrets.iter().cloned()),
            approval: action.approval.clone(),
            expected_output: action
                .observed_output
                .clone()
                .or_else(|| action.output.clone()),
            review_notes: review_notes_for_action(action),
        });
    }

    let mut parameters: Vec<WorkflowCandidateParameter> = parameter_values
        .iter()
        .map(|(name, values)| WorkflowCandidateParameter {
            name: name.clone(),
            source_paths: parameter_paths
                .get(name)
                .map(|paths| paths.iter().cloned().collect())
                .unwrap_or_default(),
            examples: values.iter().take(5).cloned().collect(),
            required: true,
        })
        .collect();
    // Caller-supplied extras (e.g. release metadata fields) take
    // precedence and are appended in stable order. De-dupe by name so a
    // caller-supplied parameter overrides any same-name parameter
    // discovered from action parameters.
    let extra_names: BTreeSet<String> = extra_parameters.iter().map(|p| p.name.clone()).collect();
    parameters.retain(|p| !extra_names.contains(&p.name));
    parameters.extend(extra_parameters);
    parameters.sort_by(|left, right| left.name.cmp(&right.name));

    let example_refs = vec![WorkflowCandidateExample {
        trace_id: trace.id.clone(),
        source_hash: trace.source_hash.clone().unwrap_or_default(),
        start_index: 0,
        action_ids: trace
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect(),
    }];

    let capabilities = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.capabilities.iter().cloned()),
    );
    let required_secrets = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.required_secrets.iter().cloned()),
    );
    let approval_points = steps
        .iter()
        .filter_map(|step| step.approval.clone())
        .collect::<Vec<_>>();
    let side_effects = sorted_side_effects(
        steps
            .iter()
            .flat_map(|step| step.side_effects.iter().cloned())
            .collect(),
    );
    let expected_outputs = steps
        .iter()
        .filter_map(|step| step.expected_output.clone())
        .collect::<Vec<_>>();
    let expected_receipts = expected_receipts_for_examples(std::slice::from_ref(trace), &[(0, 0)]);

    let savings = estimate_savings(std::slice::from_ref(trace), &[(0, 0)], &steps);
    let confidence = confidence_for(&[(0, 0)], 1, &steps, true);
    let name = options
        .workflow_name
        .clone()
        .unwrap_or_else(|| infer_workflow_name(&steps));
    let package_name = options
        .package_name
        .clone()
        .unwrap_or_else(|| name.replace('_', "-"));

    WorkflowCandidate {
        id: stable_candidate_id(&sequence, &example_refs),
        name,
        confidence,
        cluster_key: cluster_key_for_candidate(std::slice::from_ref(trace), &[(0, 0)], &steps),
        sequence_signature: sequence,
        parameters,
        steps,
        examples: example_refs.clone(),
        capabilities: capabilities.clone(),
        required_secrets: required_secrets.clone(),
        approval_points,
        side_effects,
        expected_outputs,
        expected_receipts,
        warnings,
        rejection_reasons: Vec::new(),
        promotion: PromotionMetadata {
            source_trace_hashes: example_refs
                .iter()
                .map(|example| example.source_hash.clone())
                .collect(),
            author: options.author.clone(),
            approver: options.approver.clone(),
            created_at: now_rfc3339(),
            version: "0.1.0".to_string(),
            package_name,
            capability_set: capabilities,
            secrets_required: required_secrets,
            rollback_target: Some("keep source trace and previous package version".to_string()),
            eval_pack_link: options.eval_pack_link.clone(),
            ..PromotionMetadata::default()
        },
        savings,
        shadow: ShadowRunReport::default(),
    }
}

fn review_notes_for_action(action: &CrystallizationAction) -> Vec<String> {
    let mut notes = Vec::new();
    if action.kind == "shell_failure"
        || matches!(
            action
                .metadata
                .get("success")
                .and_then(|value| value.as_bool()),
            Some(false)
        )
    {
        notes.push(format!(
            "shell/tool step '{}' failed in the source trace; reviewer should confirm \
             whether deterministic recovery is possible before promotion.",
            action.name
        ));
    }
    if let Some(hint) = action
        .metadata
        .get("recovery_hint")
        .and_then(|value| value.as_str())
        .filter(|hint| !hint.trim().is_empty())
    {
        notes.push(format!("recovery hint from source trace: {hint}"));
    }
    if action.kind == "agent_recovery_advice" {
        notes.push(
            "agent-authored recovery advice; treat as advisory, never as deterministic truth."
                .to_string(),
        );
    }
    notes
}

pub fn load_crystallization_traces_from_dir(
    dir: &Path,
) -> Result<Vec<CrystallizationTrace>, VmError> {
    let mut paths = Vec::new();
    collect_json_paths(dir, &mut paths)?;
    if paths.is_empty() {
        return Err(VmError::Runtime(format!(
            "no .json trace files found under {}",
            dir.display()
        )));
    }
    paths.sort();
    paths
        .iter()
        .map(|path| load_crystallization_trace(path))
        .collect()
}

pub fn load_crystallization_trace(path: &Path) -> Result<CrystallizationTrace, VmError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read crystallization trace {}: {error}",
            path.display()
        ))
    })?;
    let value: JsonValue = serde_json::from_str(&content).map_err(|error| {
        VmError::Runtime(format!(
            "failed to parse crystallization trace {}: {error}",
            path.display()
        ))
    })?;

    let mut trace = if value.get("actions").is_some() {
        serde_json::from_value::<CrystallizationTrace>(value.clone()).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode crystallization trace {}: {error}",
                path.display()
            ))
        })?
    } else if value.get("stages").is_some() || value.get("_type") == Some(&json!("workflow_run")) {
        let run: RunRecord = serde_json::from_value(value.clone()).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode run record {} as crystallization input: {error}",
                path.display()
            ))
        })?;
        trace_from_run_record(run)
    } else {
        return Err(VmError::Runtime(format!(
            "{} is neither a crystallization trace nor a workflow run record",
            path.display()
        )));
    };
    if trace.source.is_none() {
        trace.source = Some(path.display().to_string());
    }
    if trace.source_hash.is_none() {
        trace.source_hash = Some(hash_bytes(content.as_bytes()));
    }
    Ok(normalize_trace(trace))
}

pub fn write_crystallization_artifacts(
    mut artifacts: CrystallizationArtifacts,
    workflow_path: &Path,
    report_path: &Path,
    eval_pack_path: Option<&Path>,
) -> Result<CrystallizationReport, VmError> {
    crate::atomic_io::atomic_write(workflow_path, artifacts.harn_code.as_bytes()).map_err(
        |error| {
            VmError::Runtime(format!(
                "failed to write generated workflow {}: {error}",
                workflow_path.display()
            ))
        },
    )?;

    artifacts.report.harn_code_path = Some(workflow_path.display().to_string());
    if let Some(path) = eval_pack_path {
        if !artifacts.eval_pack_toml.trim().is_empty() {
            crate::atomic_io::atomic_write(path, artifacts.eval_pack_toml.as_bytes()).map_err(
                |error| {
                    VmError::Runtime(format!(
                        "failed to write eval pack {}: {error}",
                        path.display()
                    ))
                },
            )?;
            artifacts.report.eval_pack_path = Some(path.display().to_string());
            if let Some(candidate) = artifacts.report.candidates.first_mut() {
                candidate.promotion.eval_pack_link = Some(path.display().to_string());
            }
        }
    }

    let report_json = serde_json::to_string_pretty(&artifacts.report)
        .map_err(|error| VmError::Runtime(format!("failed to encode report JSON: {error}")))?;
    crate::atomic_io::atomic_write(report_path, report_json.as_bytes()).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write crystallization report {}: {error}",
            report_path.display()
        ))
    })?;
    Ok(artifacts.report)
}

fn collect_json_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), VmError> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read crystallization trace dir {}: {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            VmError::Runtime(format!(
                "failed to read entry in trace dir {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_paths(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn trace_from_run_record(run: RunRecord) -> CrystallizationTrace {
    let mut actions = Vec::new();
    for stage in &run.stages {
        actions.push(CrystallizationAction {
            id: stage.id.clone(),
            kind: if stage.kind.is_empty() {
                "stage".to_string()
            } else {
                stage.kind.clone()
            },
            name: stage.node_id.clone(),
            timestamp: Some(stage.started_at.clone()),
            output: stage.visible_text.as_ref().map(|text| json!(text)),
            observed_output: stage.visible_text.as_ref().map(|text| json!(text)),
            duration_ms: stage.usage.as_ref().map(|usage| usage.total_duration_ms),
            cost: stage
                .usage
                .as_ref()
                .map(|usage| CrystallizationCost {
                    model_calls: usage.call_count,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_cost_usd: usage.total_cost,
                    wall_ms: usage.total_duration_ms,
                    model: usage.models.first().cloned(),
                })
                .unwrap_or_default(),
            deterministic: Some(
                stage
                    .usage
                    .as_ref()
                    .map(|usage| usage.call_count == 0)
                    .unwrap_or(true),
            ),
            fuzzy: Some(
                stage
                    .usage
                    .as_ref()
                    .is_some_and(|usage| usage.call_count > 0),
            ),
            metadata: stage.metadata.clone(),
            ..CrystallizationAction::default()
        });
    }
    for tool in &run.tool_recordings {
        actions.push(CrystallizationAction {
            id: tool.tool_use_id.clone(),
            kind: "tool_call".to_string(),
            name: tool.tool_name.clone(),
            timestamp: Some(tool.timestamp.clone()),
            output: Some(json!(tool.result)),
            observed_output: Some(json!(tool.result)),
            duration_ms: Some(tool.duration_ms as i64),
            deterministic: Some(!tool.is_rejected),
            fuzzy: Some(false),
            metadata: BTreeMap::from([
                ("args_hash".to_string(), json!(tool.args_hash)),
                ("iteration".to_string(), json!(tool.iteration)),
                ("is_rejected".to_string(), json!(tool.is_rejected)),
            ]),
            ..CrystallizationAction::default()
        });
    }
    for question in &run.hitl_questions {
        actions.push(CrystallizationAction {
            id: question.request_id.clone(),
            kind: "human_approval".to_string(),
            name: question.agent.clone(),
            timestamp: Some(question.asked_at.clone()),
            approval: Some(CrystallizationApproval {
                prompt: Some(question.prompt.clone()),
                required: true,
                boundary: Some("hitl".to_string()),
                ..CrystallizationApproval::default()
            }),
            deterministic: Some(false),
            fuzzy: Some(false),
            metadata: question
                .trace_id
                .as_ref()
                .map(|trace_id| BTreeMap::from([("trace_id".to_string(), json!(trace_id))]))
                .unwrap_or_default(),
            ..CrystallizationAction::default()
        });
    }
    actions.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

    CrystallizationTrace {
        version: TRACE_SCHEMA_VERSION,
        id: run.id.clone(),
        workflow_id: Some(run.workflow_id.clone()),
        started_at: Some(run.started_at.clone()),
        finished_at: run.finished_at.clone(),
        actions,
        replay_run: run.replay_fixture.as_ref().map(replay_run_from_fixture),
        replay_allowlist: default_trace_replay_allowlist(),
        usage: run
            .usage
            .map(|usage| CrystallizationUsage {
                model_calls: usage.call_count,
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_cost_usd: usage.total_cost,
                wall_ms: usage.total_duration_ms,
            })
            .unwrap_or_default(),
        metadata: run.metadata.clone(),
        ..CrystallizationTrace::default()
    }
}

fn replay_run_from_fixture(fixture: &ReplayFixture) -> ReplayTraceRun {
    ReplayTraceRun {
        run_id: fixture.source_run_id.clone(),
        final_artifacts: fixture
            .stage_assertions
            .iter()
            .map(|assertion| {
                json!({
                    "node_id": assertion.node_id.clone(),
                    "expected_status": assertion.expected_status.clone(),
                    "expected_outcome": assertion.expected_outcome.clone(),
                    "expected_branch": assertion.expected_branch.clone(),
                    "required_artifact_kinds": assertion.required_artifact_kinds.clone(),
                    "visible_text_contains": assertion.visible_text_contains.clone(),
                })
            })
            .collect(),
        policy_decisions: vec![json!({
            "workflow_id": fixture.workflow_id.clone(),
            "expected_status": fixture.expected_status.clone(),
            "eval_kind": fixture.eval_kind.clone(),
        })],
        ..ReplayTraceRun::default()
    }
}

fn default_trace_replay_allowlist() -> Vec<ReplayAllowlistRule> {
    vec![ReplayAllowlistRule {
        path: "/run_id".to_string(),
        reason: "run ids are allocated per execution".to_string(),
        replacement: None,
    }]
}

fn normalize_trace(mut trace: CrystallizationTrace) -> CrystallizationTrace {
    if trace.version == 0 {
        trace.version = TRACE_SCHEMA_VERSION;
    }
    if trace.id.trim().is_empty() {
        trace.id = new_id("trace");
    }
    if trace.source_hash.is_none() {
        let payload = serde_json::to_vec(&trace.actions).unwrap_or_default();
        trace.source_hash = Some(hash_bytes(&payload));
    }
    for (idx, action) in trace.actions.iter_mut().enumerate() {
        if action.id.trim().is_empty() {
            action.id = format!("action_{}", idx + 1);
        }
        if action.kind.trim().is_empty() {
            action.kind = "action".to_string();
        }
        if action.name.trim().is_empty() {
            action.name = action.kind.clone();
        }
        action.capabilities.sort();
        action.capabilities.dedup();
        action.required_secrets.sort();
        action.required_secrets.dedup();
        action.side_effects = sorted_side_effects(std::mem::take(&mut action.side_effects));
        if action.cost.model_calls == 0 && action.kind == "model_call" {
            action.cost.model_calls = 1;
        }
    }
    if trace.usage == CrystallizationUsage::default() {
        for action in &trace.actions {
            trace.usage.model_calls += action.cost.model_calls;
            trace.usage.input_tokens += action.cost.input_tokens;
            trace.usage.output_tokens += action.cost.output_tokens;
            trace.usage.total_cost_usd += action.cost.total_cost_usd;
            trace.usage.wall_ms += action.cost.wall_ms + action.duration_ms.unwrap_or_default();
        }
    }
    trace
}

fn partition_mineable_traces(
    traces: Vec<CrystallizationTrace>,
) -> (Vec<CrystallizationTrace>, Vec<CrystallizationTrace>) {
    traces
        .into_iter()
        .partition(|trace| trace_is_mineable(trace).is_ok())
}

fn trace_is_mineable(trace: &CrystallizationTrace) -> Result<(), String> {
    if metadata_has_unresolved_policy_violation(&trace.metadata) {
        return Err("trace has unresolved policy violations".to_string());
    }
    for action in &trace.actions {
        if metadata_has_unresolved_policy_violation(&action.metadata) {
            return Err(format!(
                "action {} has unresolved policy violations",
                action.id
            ));
        }
        if action_has_nondeterministic_side_effect(action) {
            return Err(format!(
                "action {} has nondeterministic side effects",
                action.id
            ));
        }
    }
    Ok(())
}

fn metadata_has_unresolved_policy_violation(metadata: &BTreeMap<String, JsonValue>) -> bool {
    for (key, value) in metadata {
        let lower = key.to_ascii_lowercase();
        if lower.contains("unresolved_policy_violation")
            || lower == "policy_violation_unresolved"
            || lower == "policy_violation"
        {
            if value.as_bool() == Some(true) {
                return true;
            }
            if value
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case("unresolved"))
            {
                return true;
            }
            if value.as_array().is_some_and(|items| !items.is_empty()) {
                return true;
            }
        }
        if lower == "policy_status"
            && value
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case("unresolved"))
        {
            return true;
        }
    }
    false
}

fn action_has_nondeterministic_side_effect(action: &CrystallizationAction) -> bool {
    if action.side_effects.is_empty() {
        return false;
    }
    if action
        .metadata
        .get("nondeterministic_side_effect")
        .and_then(JsonValue::as_bool)
        == Some(true)
    {
        return true;
    }
    if action.deterministic == Some(false) && action.fuzzy.unwrap_or(false) {
        return true;
    }
    action.side_effects.iter().any(|effect| {
        effect
            .metadata
            .get("nondeterministic")
            .and_then(JsonValue::as_bool)
            == Some(true)
    })
}

fn mine_candidates(
    traces: &[CrystallizationTrace],
    min_examples: usize,
    options: &CrystallizeOptions,
) -> Vec<WorkflowCandidate> {
    let signatures = traces
        .iter()
        .map(|trace| {
            trace
                .actions
                .iter()
                .map(action_signature)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let Some((sequence, examples)) = best_repeated_sequence(&signatures, min_examples) else {
        return Vec::new();
    };

    let mut example_refs = Vec::new();
    for (trace_index, start_index) in &examples {
        let trace = &traces[*trace_index];
        example_refs.push(WorkflowCandidateExample {
            trace_id: trace.id.clone(),
            source_hash: trace.source_hash.clone().unwrap_or_default(),
            start_index: *start_index,
            action_ids: trace.actions[*start_index..*start_index + sequence.len()]
                .iter()
                .map(|action| action.id.clone())
                .collect(),
        });
    }

    let mut steps = Vec::new();
    let mut parameter_values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut parameter_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rejection_reasons = Vec::new();
    let mut warnings = Vec::new();

    for step_index in 0..sequence.len() {
        let actions = examples
            .iter()
            .map(|(trace_index, start_index)| {
                &traces[*trace_index].actions[*start_index + step_index]
            })
            .collect::<Vec<_>>();
        let first = actions[0];
        if !compatible_side_effects(&actions) {
            rejection_reasons.push(format!(
                "step {} '{}' has divergent side effects across examples",
                step_index + 1,
                first.name
            ));
        }

        let mut parameter_refs = BTreeSet::new();
        for action in &actions {
            for (name, value) in &action.parameters {
                if is_scalar(value) {
                    parameter_values
                        .entry(sanitize_identifier(name))
                        .or_default()
                        .insert(json_scalar_string(value));
                    parameter_paths
                        .entry(sanitize_identifier(name))
                        .or_default()
                        .insert(format!("steps[{step_index}].parameters.{name}"));
                    parameter_refs.insert(sanitize_identifier(name));
                }
            }
        }
        collect_varying_parameters(
            &actions,
            "inputs",
            |action| &action.inputs,
            &mut parameter_values,
            &mut parameter_paths,
            &mut parameter_refs,
        );

        let fuzzy = first.fuzzy.unwrap_or(false)
            || first.kind == "model_call"
            || actions.iter().any(|action| action.fuzzy.unwrap_or(false));
        if fuzzy {
            warnings.push(format!(
                "step {} '{}' remains fuzzy and requires review/LLM handling",
                step_index + 1,
                first.name
            ));
        }

        steps.push(WorkflowCandidateStep {
            index: step_index + 1,
            kind: first.kind.clone(),
            name: first.name.clone(),
            segment: if fuzzy {
                SegmentKind::Fuzzy
            } else {
                SegmentKind::Deterministic
            },
            parameter_refs: parameter_refs.into_iter().collect(),
            constants: constants_for_action(first),
            preconditions: preconditions_for_action(first),
            side_effects: first.side_effects.clone(),
            capabilities: sorted_strings(first.capabilities.iter().cloned()),
            required_secrets: sorted_strings(first.required_secrets.iter().cloned()),
            approval: first.approval.clone(),
            expected_output: stable_expected_output(&actions),
            review_notes: Vec::new(),
        });
    }

    let parameters = parameter_values
        .iter()
        .map(|(name, values)| WorkflowCandidateParameter {
            name: name.clone(),
            source_paths: parameter_paths
                .get(name)
                .map(|paths| paths.iter().cloned().collect())
                .unwrap_or_default(),
            examples: values.iter().take(5).cloned().collect(),
            required: true,
        })
        .collect::<Vec<_>>();

    let capabilities = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.capabilities.iter().cloned()),
    );
    let required_secrets = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.required_secrets.iter().cloned()),
    );
    let approval_points = steps
        .iter()
        .filter_map(|step| step.approval.clone())
        .collect::<Vec<_>>();
    let side_effects = sorted_side_effects(
        steps
            .iter()
            .flat_map(|step| step.side_effects.iter().cloned())
            .collect(),
    );
    let expected_outputs = steps
        .iter()
        .filter_map(|step| step.expected_output.clone())
        .collect::<Vec<_>>();
    let expected_receipts = expected_receipts_for_examples(traces, &examples);
    let savings = estimate_savings(traces, &examples, &steps);
    let confidence = confidence_for(
        &examples,
        traces.len(),
        &steps,
        rejection_reasons.is_empty(),
    );
    let name = options
        .workflow_name
        .clone()
        .unwrap_or_else(|| infer_workflow_name(&steps));
    let package_name = options
        .package_name
        .clone()
        .unwrap_or_else(|| name.replace('_', "-"));

    vec![WorkflowCandidate {
        id: stable_candidate_id(&sequence, &example_refs),
        name,
        confidence,
        cluster_key: cluster_key_for_candidate(traces, &examples, &steps),
        sequence_signature: sequence,
        parameters,
        steps,
        examples: example_refs.clone(),
        capabilities: capabilities.clone(),
        required_secrets: required_secrets.clone(),
        approval_points,
        side_effects,
        expected_outputs,
        expected_receipts,
        warnings,
        rejection_reasons,
        promotion: PromotionMetadata {
            source_trace_hashes: example_refs
                .iter()
                .map(|example| example.source_hash.clone())
                .collect(),
            author: options.author.clone(),
            approver: options.approver.clone(),
            created_at: now_rfc3339(),
            version: "0.1.0".to_string(),
            package_name,
            capability_set: capabilities,
            secrets_required: required_secrets,
            rollback_target: Some("keep source traces and previous package version".to_string()),
            eval_pack_link: options.eval_pack_link.clone(),
            ..PromotionMetadata::default()
        },
        savings,
        shadow: ShadowRunReport::default(),
    }]
}

fn best_repeated_sequence(
    signatures: &[Vec<String>],
    min_examples: usize,
) -> Option<RepeatedSequence> {
    let mut occurrences: BTreeMap<Vec<String>, Vec<(usize, usize)>> = BTreeMap::new();
    for (trace_index, trace_signatures) in signatures.iter().enumerate() {
        for start in 0..trace_signatures.len() {
            let max_len = (trace_signatures.len() - start).min(12);
            for len in 2..=max_len {
                let sequence = trace_signatures[start..start + len].to_vec();
                occurrences
                    .entry(sequence)
                    .or_default()
                    .push((trace_index, start));
            }
        }
    }

    occurrences
        .into_iter()
        .filter_map(|(sequence, positions)| {
            let mut seen = BTreeSet::new();
            let mut examples = Vec::new();
            for (trace_index, start) in positions {
                if seen.insert(trace_index) {
                    examples.push((trace_index, start));
                }
            }
            if examples.len() >= min_examples {
                Some((sequence, examples))
            } else {
                None
            }
        })
        .max_by(
            |(left_sequence, left_examples), (right_sequence, right_examples)| {
                left_examples
                    .len()
                    .cmp(&right_examples.len())
                    .then_with(|| left_sequence.len().cmp(&right_sequence.len()))
            },
        )
}

fn action_signature(action: &CrystallizationAction) -> String {
    let mut parameter_keys = action.parameters.keys().cloned().collect::<Vec<_>>();
    parameter_keys.sort();
    format!(
        "{}:{}:{}",
        action.kind,
        action.name,
        parameter_keys.join(",")
    )
}

fn compatible_side_effects(actions: &[&CrystallizationAction]) -> bool {
    let first = sorted_side_effects(actions[0].side_effects.clone());
    actions
        .iter()
        .skip(1)
        .all(|action| sorted_side_effects(action.side_effects.clone()) == first)
}

fn collect_varying_parameters(
    actions: &[&CrystallizationAction],
    root: &str,
    value_for: impl Fn(&CrystallizationAction) -> &JsonValue,
    parameter_values: &mut BTreeMap<String, BTreeSet<String>>,
    parameter_paths: &mut BTreeMap<String, BTreeSet<String>>,
    parameter_refs: &mut BTreeSet<String>,
) {
    let mut paths = BTreeMap::<String, Vec<JsonValue>>::new();
    for action in actions {
        collect_scalar_paths(value_for(action), root, &mut paths);
    }
    for (path, values) in paths {
        if values.len() != actions.len() {
            continue;
        }
        let unique = values
            .iter()
            .map(json_scalar_string)
            .collect::<BTreeSet<_>>();
        if unique.len() < 2 {
            continue;
        }
        let name = parameter_name_for_path(&path);
        parameter_values
            .entry(name.clone())
            .or_default()
            .extend(unique);
        parameter_paths
            .entry(name.clone())
            .or_default()
            .insert(path);
        parameter_refs.insert(name);
    }
}

fn collect_scalar_paths(
    value: &JsonValue,
    prefix: &str,
    out: &mut BTreeMap<String, Vec<JsonValue>>,
) {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                collect_scalar_paths(child, &format!("{prefix}.{key}"), out);
            }
        }
        JsonValue::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                collect_scalar_paths(child, &format!("{prefix}[{idx}]"), out);
            }
        }
        _ if is_scalar(value) => {
            out.entry(prefix.to_string())
                .or_default()
                .push(value.clone());
        }
        _ => {}
    }
}

fn parameter_name_for_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    for (needle, name) in [
        ("version", "version"),
        ("repo_path", "repo_path"),
        ("repo", "repo_path"),
        ("branch_name", "branch_name"),
        ("branch", "branch_name"),
        ("release_target", "release_target"),
        ("target", "release_target"),
    ] {
        if lower.contains(needle) {
            return name.to_string();
        }
    }
    let tail = path
        .rsplit(['.', '['])
        .next()
        .unwrap_or("param")
        .trim_end_matches(']');
    sanitize_identifier(tail)
}

fn constants_for_action(action: &CrystallizationAction) -> BTreeMap<String, JsonValue> {
    let mut constants = BTreeMap::new();
    constants.insert("kind".to_string(), json!(action.kind));
    constants.insert("name".to_string(), json!(action.name));
    if action.deterministic.unwrap_or(false) {
        constants.insert("deterministic".to_string(), json!(true));
    }
    constants
}

fn preconditions_for_action(action: &CrystallizationAction) -> Vec<String> {
    let mut out = Vec::new();
    for capability in &action.capabilities {
        out.push(format!("capability '{capability}' is available"));
    }
    for secret in &action.required_secrets {
        out.push(format!("secret '{secret}' is configured"));
    }
    if let Some(approval) = &action.approval {
        if approval.required {
            out.push("human approval boundary is preserved".to_string());
        }
    }
    out
}

fn stable_expected_output(actions: &[&CrystallizationAction]) -> Option<JsonValue> {
    let first = actions[0]
        .observed_output
        .as_ref()
        .or(actions[0].output.as_ref())?;
    if actions
        .iter()
        .all(|action| action.observed_output.as_ref().or(action.output.as_ref()) == Some(first))
    {
        Some(first.clone())
    } else {
        None
    }
}

fn expected_receipts_for_examples(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
) -> Vec<JsonValue> {
    examples
        .iter()
        .find_map(|(trace_index, _)| {
            traces
                .get(*trace_index)
                .and_then(|trace| trace.replay_run.as_ref())
                .map(|run| run.effect_receipts.clone())
                .filter(|receipts| !receipts.is_empty())
        })
        .unwrap_or_default()
}

fn cluster_key_for_candidate(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
    steps: &[WorkflowCandidateStep],
) -> WorkflowClusterKey {
    let goal = examples.iter().find_map(|(trace_index, _)| {
        traces.get(*trace_index).and_then(|trace| {
            trace
                .metadata
                .get("goal")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .or_else(|| trace.workflow_id.clone())
        })
    });
    let tool_sequence = steps
        .iter()
        .filter(|step| step.kind == "tool_call" || step.kind == "file_mutation")
        .map(|step| step.name.clone())
        .collect::<Vec<_>>();
    let touched_artifact_types = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.side_effects.iter().map(artifact_type_for_side_effect)),
    );
    let success_criteria = sorted_strings(examples.iter().filter_map(|(trace_index, _)| {
        traces
            .get(*trace_index)
            .and_then(|trace| trace.metadata.get("success_criteria"))
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    }));

    WorkflowClusterKey {
        goal,
        tool_sequence,
        touched_artifact_types,
        success_criteria,
    }
}

fn artifact_type_for_side_effect(effect: &CrystallizationSideEffect) -> String {
    if let Some(kind) = effect
        .metadata
        .get("artifact_type")
        .and_then(JsonValue::as_str)
        .filter(|kind| !kind.trim().is_empty())
    {
        return kind.to_string();
    }
    let lower_kind = effect.kind.to_ascii_lowercase();
    if lower_kind.contains("git") {
        "git".to_string()
    } else if lower_kind.contains("file") {
        Path::new(&effect.target)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!("file:{ext}"))
            .unwrap_or_else(|| "file".to_string())
    } else if lower_kind.contains("receipt") {
        "receipt".to_string()
    } else if lower_kind.contains("publish") {
        "package_publish".to_string()
    } else if !lower_kind.is_empty() {
        lower_kind
    } else {
        "unknown".to_string()
    }
}

fn refresh_promotion_metadata(
    candidate: &mut WorkflowCandidate,
    min_examples: usize,
    options: &CrystallizeOptions,
) {
    let min_confidence = if options.promotion_min_confidence > 0.0 {
        options.promotion_min_confidence
    } else {
        DEFAULT_PROMOTION_MIN_CONFIDENCE
    };
    let shadow_success_count = candidate
        .shadow
        .traces
        .iter()
        .filter(|trace| trace.pass)
        .count();
    let shadow_failure_count = candidate
        .shadow
        .traces
        .len()
        .saturating_sub(shadow_success_count);
    let sample_count = candidate
        .shadow
        .compared_traces
        .max(candidate.examples.len());
    let divergence_history = candidate
        .shadow
        .traces
        .iter()
        .filter(|trace| !trace.pass)
        .flat_map(divergence_records_for_shadow_trace)
        .collect::<Vec<_>>();
    let source_trace_hashes = sorted_strings(
        candidate
            .promotion
            .source_trace_hashes
            .iter()
            .cloned()
            .chain(
                candidate
                    .shadow
                    .traces
                    .iter()
                    .map(|trace| trace.source_hash.clone()),
            ),
    );
    let approval_history = options
        .approver
        .as_ref()
        .filter(|approver| !approver.trim().is_empty())
        .map(|approver| {
            vec![PromotionApprovalRecord {
                actor: approver.clone(),
                decision: "approved_for_shadow_promotion".to_string(),
                recorded_at: now_rfc3339(),
                reason: Some("approver supplied in crystallization options".to_string()),
            }]
        })
        .unwrap_or_default();
    let mut criteria = PromotionCriteria {
        min_examples,
        min_confidence,
        requires_shadow_pass: true,
        requires_no_rejections: true,
        requires_human_approval: true,
        approval_reason: Some(
            "workflow candidates start in shadow mode and require explicit human approval before promotion".to_string(),
        ),
        ..PromotionCriteria::default()
    };
    if sample_count < min_examples {
        criteria.reasons.push(format!(
            "sample count {} is below required minimum {min_examples}",
            sample_count
        ));
    }
    if candidate.confidence < min_confidence {
        criteria.reasons.push(format!(
            "confidence {:.2} is below required minimum {:.2}",
            candidate.confidence, min_confidence
        ));
    }
    if !candidate.shadow.pass {
        criteria
            .reasons
            .push("shadow comparison did not pass".to_string());
    }
    if !candidate.rejection_reasons.is_empty() {
        criteria
            .reasons
            .push("candidate has rejection reasons".to_string());
    }
    if approval_history.is_empty() {
        criteria
            .reasons
            .push("human approval has not been recorded".to_string());
    }
    criteria.status = if !candidate.rejection_reasons.is_empty()
        || !candidate.shadow.pass
        || sample_count < min_examples
        || candidate.confidence < min_confidence
    {
        PromotionStatus::Blocked
    } else if approval_history.is_empty() {
        PromotionStatus::NeedsApproval
    } else {
        PromotionStatus::Ready
    };

    candidate.promotion.sample_count = sample_count;
    candidate.promotion.source_trace_hashes = source_trace_hashes;
    candidate.promotion.confidence = candidate.confidence;
    candidate.promotion.shadow_success_count = shadow_success_count;
    candidate.promotion.shadow_failure_count = shadow_failure_count;
    candidate.promotion.divergence_history = divergence_history;
    candidate.promotion.approval_history = approval_history;
    candidate.promotion.criteria = criteria;
    candidate.promotion.estimated_time_token_savings = candidate.savings.clone();
}

fn divergence_records_for_shadow_trace(
    trace: &ShadowTraceResult,
) -> Vec<PromotionDivergenceRecord> {
    if let Some(report) = &trace.replay_oracle {
        if let Some(divergence) = &report.divergence {
            return vec![PromotionDivergenceRecord {
                trace_id: trace.trace_id.clone(),
                path: Some(divergence.path.clone()),
                message: divergence.message.clone(),
                left: Some(divergence.left.clone()),
                right: Some(divergence.right.clone()),
            }];
        }
    }
    trace
        .details
        .iter()
        .map(|detail| PromotionDivergenceRecord {
            trace_id: trace.trace_id.clone(),
            path: None,
            message: detail.clone(),
            left: None,
            right: None,
        })
        .collect()
}

fn shadow_candidate(
    candidate: &WorkflowCandidate,
    traces: &[CrystallizationTrace],
) -> ShadowRunReport {
    let mut failures = Vec::new();
    let mut results = Vec::new();
    let mut compared_trace_ids = BTreeSet::new();
    for example in &candidate.examples {
        let Some(trace) = traces.iter().find(|trace| trace.id == example.trace_id) else {
            failures.push(format!("missing source trace {}", example.trace_id));
            continue;
        };
        compared_trace_ids.insert(trace.id.clone());
        let result = shadow_trace_result(candidate, trace, example.start_index);
        if !result.pass {
            failures.push(format!("trace {} failed shadow comparison", trace.id));
        }
        results.push(result);
    }

    for trace in traces {
        if compared_trace_ids.contains(&trace.id) {
            continue;
        }
        if let Some(start_index) = find_sequence_start(trace, &candidate.sequence_signature) {
            compared_trace_ids.insert(trace.id.clone());
            let result = shadow_trace_result(candidate, trace, start_index);
            if !result.pass {
                failures.push(format!("trace {} failed shadow comparison", trace.id));
            }
            results.push(result);
        }
    }

    ShadowRunReport {
        pass: failures.is_empty(),
        compared_traces: results.len(),
        failures,
        traces: results,
    }
}

fn find_sequence_start(trace: &CrystallizationTrace, sequence: &[String]) -> Option<usize> {
    if sequence.is_empty() || trace.actions.len() < sequence.len() {
        return None;
    }
    trace
        .actions
        .windows(sequence.len())
        .position(|window| window.iter().map(action_signature).collect::<Vec<_>>() == sequence)
}

fn shadow_trace_result(
    candidate: &WorkflowCandidate,
    trace: &CrystallizationTrace,
    start_index: usize,
) -> ShadowTraceResult {
    let mut details = Vec::new();
    let end = start_index + candidate.steps.len();
    if end > trace.actions.len() {
        details.push("candidate sequence extends past trace action list".to_string());
    } else {
        let signatures = trace.actions[start_index..end]
            .iter()
            .map(action_signature)
            .collect::<Vec<_>>();
        if signatures != candidate.sequence_signature {
            details.push("action sequence signature changed".to_string());
        }
        for (offset, step) in candidate.steps.iter().enumerate() {
            let action = &trace.actions[start_index + offset];
            if sorted_side_effects(action.side_effects.clone()) != step.side_effects {
                details.push(format!(
                    "step {} side effects differ for action {}",
                    step.index, action.id
                ));
            }
            if action.approval.as_ref().map(|approval| approval.required)
                != step.approval.as_ref().map(|approval| approval.required)
            {
                details.push(format!("step {} approval boundary differs", step.index));
            }
            if step.segment == SegmentKind::Deterministic {
                if let Some(expected) = &step.expected_output {
                    let actual = action.observed_output.as_ref().or(action.output.as_ref());
                    if actual != Some(expected) {
                        details.push(format!("step {} deterministic output differs", step.index));
                    }
                }
            }
        }
    }

    let (replay_oracle, compared_receipts) = replay_oracle_for_shadow(candidate, trace);
    if let Some(report) = &replay_oracle {
        if !report.passed {
            if let Some(divergence) = &report.divergence {
                details.push(format!(
                    "receipt replay diverged at {}: {}",
                    divergence.path, divergence.message
                ));
            } else {
                details.push("receipt replay oracle did not pass".to_string());
            }
        }
    }

    ShadowTraceResult {
        trace_id: trace.id.clone(),
        source_hash: trace.source_hash.clone().unwrap_or_default(),
        pass: details.is_empty(),
        details,
        compared_receipts,
        replay_oracle,
    }
}

fn replay_oracle_for_shadow(
    candidate: &WorkflowCandidate,
    trace: &CrystallizationTrace,
) -> (Option<ReplayOracleReport>, usize) {
    let Some(first_run) = trace.replay_run.as_ref() else {
        return (None, 0);
    };
    if first_run.effect_receipts.is_empty() && candidate.expected_receipts.is_empty() {
        return (None, 0);
    }
    let mut second_run = first_run.clone();
    second_run.run_id = format!("shadow_{}_{}", candidate.id, trace.id);
    second_run.effect_receipts = candidate.expected_receipts.clone();
    let compared_receipts = first_run
        .effect_receipts
        .len()
        .max(second_run.effect_receipts.len());
    let oracle_trace = ReplayOracleTrace {
        name: format!("{}_shadow_receipts_{}", candidate.name, trace.id),
        description: Some(
            "crystallization shadow receipt comparison using the replay oracle".to_string(),
        ),
        expect: ReplayExpectation::Match,
        allowlist: trace.replay_allowlist.clone(),
        first_run: first_run.clone(),
        second_run,
        ..ReplayOracleTrace::default()
    };
    let oracle_name = oracle_trace.name.clone();
    let second_run_counts = oracle_trace.second_run.counts();
    let report = match run_replay_oracle_trace(&oracle_trace) {
        Ok(report) => report,
        Err(error) => ReplayOracleReport {
            name: oracle_name,
            expectation: ReplayExpectation::Match,
            passed: false,
            first_run_counts: first_run.counts(),
            second_run_counts,
            protocol_fixture_refs: Vec::new(),
            divergence: Some(super::ReplayDivergence {
                path: "/".to_string(),
                left: JsonValue::Null,
                right: JsonValue::Null,
                message: error.to_string(),
            }),
        },
    };
    (Some(report), compared_receipts)
}

fn estimate_savings(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
    steps: &[WorkflowCandidateStep],
) -> SavingsEstimate {
    let mut estimate = SavingsEstimate::default();
    for (trace_index, start_index) in examples {
        let trace = &traces[*trace_index];
        for action in &trace.actions[*start_index..*start_index + steps.len()] {
            if action.kind == "model_call" || action.fuzzy.unwrap_or(false) {
                estimate.remaining_model_calls += action.cost.model_calls.max(1);
            } else {
                estimate.model_calls_avoided += action.cost.model_calls;
                estimate.input_tokens_avoided += action.cost.input_tokens;
                estimate.output_tokens_avoided += action.cost.output_tokens;
                estimate.estimated_cost_usd_avoided += action.cost.total_cost_usd;
                estimate.wall_ms_avoided +=
                    action.cost.wall_ms + action.duration_ms.unwrap_or_default();
            }
        }
    }
    estimate.cpu_runtime_cost_usd = 0.0;
    estimate
}

fn confidence_for(
    examples: &[(usize, usize)],
    trace_count: usize,
    steps: &[WorkflowCandidateStep],
    safe: bool,
) -> f64 {
    if !safe || trace_count == 0 {
        return 0.0;
    }
    let coverage = examples.len() as f64 / trace_count as f64;
    let deterministic = steps
        .iter()
        .filter(|step| step.segment == SegmentKind::Deterministic)
        .count() as f64
        / steps.len().max(1) as f64;
    ((coverage * 0.65) + (deterministic * 0.35)).min(0.99)
}

fn infer_workflow_name(steps: &[WorkflowCandidateStep]) -> String {
    let names = steps
        .iter()
        .map(|step| step.name.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    if names.contains("version") || names.contains("release") {
        "crystallized_version_bump".to_string()
    } else {
        "crystallized_workflow".to_string()
    }
}

pub fn generate_harn_code(candidate: &WorkflowCandidate) -> String {
    let mut out = String::new();
    let params = if candidate.parameters.is_empty() {
        "task".to_string()
    } else {
        candidate
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(out, "/**").unwrap();
    writeln!(
        out,
        " * Generated by harn crystallize. Review before promotion."
    )
    .unwrap();
    writeln!(out, " * Candidate: {}", candidate.id).unwrap();
    writeln!(
        out,
        " * Source trace hashes: {}",
        candidate.promotion.source_trace_hashes.join(", ")
    )
    .unwrap();
    writeln!(
        out,
        " * Capabilities: {}",
        if candidate.capabilities.is_empty() {
            "none".to_string()
        } else {
            candidate.capabilities.join(", ")
        }
    )
    .unwrap();
    writeln!(
        out,
        " * Required secrets: {}",
        if candidate.required_secrets.is_empty() {
            "none".to_string()
        } else {
            candidate.required_secrets.join(", ")
        }
    )
    .unwrap();
    writeln!(out, " */").unwrap();
    writeln!(out, "pipeline {}({}) {{", candidate.name, params).unwrap();
    writeln!(out, "  let review_warnings = []").unwrap();
    for step in &candidate.steps {
        writeln!(out, "  // Step {}: {} {}", step.index, step.kind, step.name).unwrap();
        for side_effect in &step.side_effects {
            writeln!(
                out,
                "  // side_effect: {} {}",
                side_effect.kind, side_effect.target
            )
            .unwrap();
        }
        if let Some(approval) = &step.approval {
            if approval.required {
                writeln!(
                    out,
                    "  // approval_required: {}",
                    approval
                        .boundary
                        .clone()
                        .unwrap_or_else(|| "human_review".to_string())
                )
                .unwrap();
            }
        }
        if step.segment == SegmentKind::Fuzzy {
            writeln!(
                out,
                "  // TODO: fuzzy segment still needs LLM/reviewer handling before deterministic promotion."
            )
            .unwrap();
            writeln!(
                out,
                "  review_warnings.push(\"fuzzy step: {}\")",
                escape_harn_string(&step.name)
            )
            .unwrap();
        }
        writeln!(
            out,
            "  log(\"crystallized step {}: {}\")",
            step.index,
            escape_harn_string(&step.name)
        )
        .unwrap();
    }
    writeln!(
        out,
        "  return {{status: \"shadow_ready\", candidate_id: \"{}\", review_warnings: review_warnings}}",
        escape_harn_string(&candidate.id)
    )
    .unwrap();
    writeln!(out, "}}").unwrap();
    out
}

fn rejected_workflow_stub(rejected: &[WorkflowCandidate]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "// Generated by harn crystallize. No safe candidate was proposed."
    )
    .unwrap();
    writeln!(out, "pipeline crystallized_workflow(task) {{").unwrap();
    writeln!(out, "  log(\"no safe crystallization candidate\")").unwrap();
    writeln!(
        out,
        "  return {{status: \"rejected\", rejected_candidates: {}}}",
        rejected.len()
    )
    .unwrap();
    writeln!(out, "}}").unwrap();
    out
}

pub fn generate_eval_pack(candidate: &WorkflowCandidate) -> String {
    let mut out = String::new();
    writeln!(out, "version = 1").unwrap();
    writeln!(
        out,
        "id = \"{}-crystallization\"",
        candidate.promotion.package_name
    )
    .unwrap();
    writeln!(
        out,
        "name = \"{} crystallization shadow evals\"",
        candidate.name
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "[package]").unwrap();
    writeln!(out, "name = \"{}\"", candidate.promotion.package_name).unwrap();
    writeln!(out, "version = \"{}\"", candidate.promotion.version).unwrap();
    writeln!(out).unwrap();
    for example in &candidate.examples {
        writeln!(out, "[[fixtures]]").unwrap();
        writeln!(out, "id = \"{}\"", example.trace_id).unwrap();
        writeln!(out, "kind = \"jsonl-trace\"").unwrap();
        writeln!(out, "trace_id = \"{}\"", example.trace_id).unwrap();
        writeln!(out).unwrap();
    }
    writeln!(out, "[[rubrics]]").unwrap();
    writeln!(out, "id = \"shadow-determinism\"").unwrap();
    writeln!(out, "kind = \"deterministic\"").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "[[rubrics.assertions]]").unwrap();
    writeln!(out, "kind = \"crystallization-shadow\"").unwrap();
    writeln!(
        out,
        "expected = {{ candidate_id = \"{}\", pass = true }}",
        candidate.id
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "[[cases]]").unwrap();
    writeln!(out, "id = \"{}-shadow\"", candidate.name).unwrap();
    writeln!(out, "name = \"{} shadow replay\"", candidate.name).unwrap();
    writeln!(out, "rubrics = [\"shadow-determinism\"]").unwrap();
    writeln!(out, "severity = \"blocking\"").unwrap();
    out
}

fn stable_candidate_id(sequence: &[String], examples: &[WorkflowCandidateExample]) -> String {
    let mut hasher = Sha256::new();
    for item in sequence {
        hasher.update(item.as_bytes());
        hasher.update([0]);
    }
    for example in examples {
        hasher.update(example.source_hash.as_bytes());
        hasher.update([0]);
    }
    format!("candidate_{}", hex_prefix(hasher.finalize().as_slice(), 16))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    hex::encode(bytes).chars().take(chars).collect::<String>()
}

fn sorted_strings(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut set = items.collect::<BTreeSet<_>>();
    set.retain(|item| !item.trim().is_empty());
    set.into_iter().collect()
}

fn sorted_side_effects(items: Vec<CrystallizationSideEffect>) -> Vec<CrystallizationSideEffect> {
    let mut items = items
        .into_iter()
        .filter(|item| !item.kind.trim().is_empty() || !item.target.trim().is_empty())
        .collect::<Vec<_>>();
    items.sort_by_key(side_effect_sort_key);
    items.dedup_by(|left, right| side_effect_sort_key(left) == side_effect_sort_key(right));
    items
}

fn side_effect_sort_key(item: &CrystallizationSideEffect) -> String {
    format!(
        "{}\x1f{}\x1f{}\x1f{}",
        item.kind,
        item.target,
        item.capability.clone().unwrap_or_default(),
        item.mutation.clone().unwrap_or_default()
    )
}

fn is_scalar(value: &JsonValue) -> bool {
    matches!(
        value,
        JsonValue::String(_) | JsonValue::Number(_) | JsonValue::Bool(_) | JsonValue::Null
    )
}

fn json_scalar_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn sanitize_identifier(raw: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in raw.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if idx == 0 && ch.is_ascii_digit() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "param".to_string()
    } else {
        trimmed
    }
}

fn escape_harn_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

// ===== Crystallization bundle =====
//
// A bundle is a directory layout that Harn writes and Harn Cloud (or any
// other importer) reads without bespoke glue. The contract is:
//
//   bundle/
//     candidate.json        # versioned manifest documented below
//     workflow.harn         # generated/reviewable workflow code
//     report.json           # full mining/shadow/eval report
//     harn.eval.toml        # generated eval pack when available (optional)
//     fixtures/             # redacted replay fixtures referenced by the
//                           # report (optional, only when --bundle is used
//                           # with `harn crystallize` and traces were on disk)
//
// `candidate.json` is the authoritative manifest. It must include the
// `schema` and `schema_version` markers. Cloud importers MUST reject any
// bundle whose `schema` is not exactly `harn.crystallization.candidate.bundle`
// or whose `schema_version` is greater than the highest version they
// understand. Only the documented additive fields may be added without
// bumping `schema_version`.

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleGenerator {
    pub tool: String,
    pub version: String,
}

impl Default for BundleGenerator {
    fn default() -> Self {
        Self {
            tool: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleWorkflowRef {
    /// Relative path inside the bundle directory.
    pub path: String,
    /// Short identifier used in `pipeline NAME(...)`.
    pub name: String,
    /// Logical package name promotion uses to register the workflow.
    pub package_name: String,
    /// Initial workflow version proposed for promotion.
    pub package_version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleSourceTrace {
    pub trace_id: String,
    pub source_hash: String,
    /// Optional human-visible URL (PR, issue, run record path) for the
    /// trace. `None` when the trace was loaded from an in-memory store.
    pub source_url: Option<String>,
    /// Optional cloud-side receipt id when the trace was already promoted
    /// into a tenant receipt. Cloud importers use this to wire candidate
    /// evidence to existing receipts without round-tripping the raw payload.
    pub source_receipt_id: Option<String>,
    /// Relative path of the redacted fixture inside the bundle, if one
    /// was emitted.
    pub fixture_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BundleStep {
    pub index: usize,
    pub kind: String,
    pub name: String,
    pub segment: SegmentKind,
    pub parameter_refs: Vec<String>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval: Option<CrystallizationApproval>,
    pub review_notes: Vec<String>,
}

impl BundleStep {
    fn from_candidate_step(step: &WorkflowCandidateStep) -> Self {
        Self {
            index: step.index,
            kind: step.kind.clone(),
            name: step.name.clone(),
            segment: step.segment.clone(),
            parameter_refs: step.parameter_refs.clone(),
            side_effects: step.side_effects.clone(),
            capabilities: step.capabilities.clone(),
            required_secrets: step.required_secrets.clone(),
            approval: step.approval.clone(),
            review_notes: step.review_notes.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleEvalPackRef {
    /// Relative path of the eval pack inside the bundle directory.
    pub path: String,
    /// Optional external link the eval pack also lives at (e.g. a hosted
    /// `eval-pack://` URI when the bundle was promoted into a tenant).
    pub link: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleFixtureRef {
    pub path: String,
    pub trace_id: String,
    pub source_hash: String,
    pub redacted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BundlePromotion {
    pub owner: Option<String>,
    pub approver: Option<String>,
    pub author: Option<String>,
    /// Logical rollout strategy. Defaults to `shadow_then_canary`. Hosted
    /// surfaces may extend this enum but must keep existing values stable.
    pub rollout_policy: String,
    pub rollback_target: Option<String>,
    pub created_at: String,
    pub workflow_version: String,
    pub package_name: String,
    pub sample_count: usize,
    pub confidence: f64,
    pub shadow_success_count: usize,
    pub shadow_failure_count: usize,
    pub divergence_history: Vec<PromotionDivergenceRecord>,
    pub approval_history: Vec<PromotionApprovalRecord>,
    pub criteria: PromotionCriteria,
    pub estimated_time_token_savings: SavingsEstimate,
}

impl Default for BundlePromotion {
    fn default() -> Self {
        Self {
            owner: None,
            approver: None,
            author: None,
            rollout_policy: DEFAULT_ROLLOUT_POLICY.to_string(),
            rollback_target: None,
            created_at: String::new(),
            workflow_version: String::new(),
            package_name: String::new(),
            sample_count: 0,
            confidence: 0.0,
            shadow_success_count: 0,
            shadow_failure_count: 0,
            divergence_history: Vec::new(),
            approval_history: Vec::new(),
            criteria: PromotionCriteria::default(),
            estimated_time_token_savings: SavingsEstimate::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleRedactionSummary {
    pub applied: bool,
    pub rules: Vec<String>,
    pub summary: String,
    /// Number of fixture files copied into the bundle (0 when no fixture
    /// directory was emitted).
    pub fixture_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationBundleManifest {
    pub schema: String,
    pub schema_version: u32,
    pub generated_at: String,
    pub generator: BundleGenerator,
    pub kind: BundleKind,
    pub candidate_id: String,
    pub external_key: String,
    pub title: String,
    pub team: Option<String>,
    pub repo: Option<String>,
    pub risk_level: String,
    pub workflow: BundleWorkflowRef,
    pub source_trace_hashes: Vec<String>,
    pub source_traces: Vec<BundleSourceTrace>,
    pub deterministic_steps: Vec<BundleStep>,
    pub fuzzy_steps: Vec<BundleStep>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub savings: SavingsEstimate,
    pub shadow: ShadowRunReport,
    pub eval_pack: Option<BundleEvalPackRef>,
    pub fixtures: Vec<BundleFixtureRef>,
    pub promotion: BundlePromotion,
    pub redaction: BundleRedactionSummary,
    pub confidence: f64,
    pub rejection_reasons: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleKind {
    /// A normal candidate that passed shadow comparison and is ready for
    /// review and promotion.
    #[default]
    Candidate,
    /// A "plan-only" candidate: every step has a side-effect-free, in-process
    /// outcome (e.g. classify and write a receipt). Cloud importers can
    /// promote these without explicit external-side-effect approval.
    PlanOnly,
    /// No safe candidate was selected. The bundle still records what was
    /// attempted, the rejection reasons, and any rejected candidates so
    /// reviewers can debug or feed it back into mining.
    Rejected,
}

#[derive(Clone, Debug, Default)]
pub struct BundleOptions {
    /// Stable identifier downstream cloud importers use to dedupe bundles
    /// across runs (defaults to a sanitized workflow name).
    pub external_key: Option<String>,
    pub title: Option<String>,
    pub team: Option<String>,
    pub repo: Option<String>,
    pub risk_level: Option<String>,
    pub rollout_policy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrystallizationBundle {
    pub manifest: CrystallizationBundleManifest,
    pub report: CrystallizationReport,
    pub harn_code: String,
    pub eval_pack_toml: String,
    pub fixtures: Vec<CrystallizationTrace>,
}

/// Errors surfaced when validating a bundle on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleValidation {
    pub bundle_dir: String,
    pub schema: String,
    pub schema_version: u32,
    pub kind: BundleKind,
    pub candidate_id: String,
    pub manifest_ok: bool,
    pub workflow_ok: bool,
    pub report_ok: bool,
    pub eval_pack_ok: bool,
    pub fixtures_ok: bool,
    pub redaction_ok: bool,
    pub problems: Vec<String>,
}

impl BundleValidation {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Build an in-memory bundle from already-mined artifacts. The traces
/// passed here are the same normalized traces used to mine the candidate;
/// they will be redacted before being attached as fixtures.
pub fn build_crystallization_bundle(
    artifacts: CrystallizationArtifacts,
    traces: &[CrystallizationTrace],
    options: BundleOptions,
) -> Result<CrystallizationBundle, VmError> {
    let CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    } = artifacts;

    let (selected, kind) = match report
        .selected_candidate_id
        .as_deref()
        .and_then(|id| report.candidates.iter().find(|c| c.id == id))
    {
        Some(candidate) => {
            let kind = if candidate_is_plan_only(candidate) {
                BundleKind::PlanOnly
            } else {
                BundleKind::Candidate
            };
            (Some(candidate), kind)
        }
        None => (None, BundleKind::Rejected),
    };

    let workflow_name = selected
        .map(|candidate| candidate.name.clone())
        .unwrap_or_else(|| "crystallized_workflow".to_string());
    let package_name = selected
        .map(|candidate| candidate.promotion.package_name.clone())
        .unwrap_or_else(|| workflow_name.replace('_', "-"));
    let workflow_version = selected
        .map(|candidate| candidate.promotion.version.clone())
        .unwrap_or_else(|| "0.0.0".to_string());

    let manifest_workflow = BundleWorkflowRef {
        path: BUNDLE_WORKFLOW_FILE.to_string(),
        name: workflow_name.clone(),
        package_name: package_name.clone(),
        package_version: workflow_version.clone(),
    };

    let external_key = options
        .external_key
        .clone()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| sanitize_external_key(&workflow_name));
    let title = options
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| infer_bundle_title(selected, &workflow_name));
    let risk_level = options
        .risk_level
        .clone()
        .filter(|risk| !risk.trim().is_empty())
        .unwrap_or_else(|| infer_risk_level(selected));
    let rollout_policy = options
        .rollout_policy
        .clone()
        .filter(|policy| !policy.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ROLLOUT_POLICY.to_string());

    let (deterministic_steps, fuzzy_steps) = match selected {
        Some(candidate) => candidate
            .steps
            .iter()
            .map(BundleStep::from_candidate_step)
            .partition::<Vec<_>, _>(|step| step.segment == SegmentKind::Deterministic),
        None => (Vec::new(), Vec::new()),
    };

    let source_trace_hashes = selected
        .map(|candidate| candidate.promotion.source_trace_hashes.clone())
        .unwrap_or_default();

    let mut source_traces = Vec::new();
    let mut fixture_refs = Vec::new();
    let mut fixture_payloads = Vec::new();
    if let Some(candidate) = selected {
        let mut fixture_trace_ids = BTreeSet::new();
        for example in &candidate.examples {
            fixture_trace_ids.insert(example.trace_id.clone());
        }
        for trace in traces {
            if find_sequence_start(trace, &candidate.sequence_signature).is_some() {
                fixture_trace_ids.insert(trace.id.clone());
            }
        }
        for trace_id in fixture_trace_ids {
            let trace = traces.iter().find(|trace| trace.id == trace_id);
            let source_hash = trace
                .and_then(|trace| trace.source_hash.clone())
                .or_else(|| {
                    candidate
                        .examples
                        .iter()
                        .find(|example| example.trace_id == trace_id)
                        .map(|example| example.source_hash.clone())
                })
                .unwrap_or_default();
            let fixture_relative = trace.map(|trace| {
                format!(
                    "{BUNDLE_FIXTURES_DIR}/{}.json",
                    sanitize_fixture_name(&trace.id)
                )
            });
            source_traces.push(BundleSourceTrace {
                trace_id: trace_id.clone(),
                source_hash: source_hash.clone(),
                source_url: trace.and_then(|trace| trace.source.clone()),
                source_receipt_id: trace
                    .and_then(|trace| trace.metadata.get("source_receipt_id"))
                    .and_then(|value| value.as_str().map(str::to_string)),
                fixture_path: fixture_relative.clone(),
            });
            if let (Some(trace), Some(fixture_path)) = (trace, fixture_relative.clone()) {
                let mut redacted = trace.clone();
                redact_trace_for_bundle(&mut redacted);
                fixture_refs.push(BundleFixtureRef {
                    path: fixture_path,
                    trace_id: trace.id.clone(),
                    source_hash,
                    redacted: true,
                });
                fixture_payloads.push(redacted);
            }
        }
    }

    // Owner defaults to author so cloud importers always have a populated
    // ownership pointer, but stays separate from `author` so reviewers can
    // assign a different owner in the manifest before promotion.
    let author = selected.and_then(|candidate| candidate.promotion.author.clone());
    let promotion = BundlePromotion {
        owner: author.clone(),
        approver: selected.and_then(|candidate| candidate.promotion.approver.clone()),
        author,
        rollout_policy,
        rollback_target: selected.and_then(|candidate| candidate.promotion.rollback_target.clone()),
        created_at: now_rfc3339(),
        workflow_version,
        package_name: package_name.clone(),
        sample_count: selected
            .map(|candidate| candidate.promotion.sample_count)
            .unwrap_or_default(),
        confidence: selected
            .map(|candidate| candidate.promotion.confidence)
            .unwrap_or_default(),
        shadow_success_count: selected
            .map(|candidate| candidate.promotion.shadow_success_count)
            .unwrap_or_default(),
        shadow_failure_count: selected
            .map(|candidate| candidate.promotion.shadow_failure_count)
            .unwrap_or_default(),
        divergence_history: selected
            .map(|candidate| candidate.promotion.divergence_history.clone())
            .unwrap_or_default(),
        approval_history: selected
            .map(|candidate| candidate.promotion.approval_history.clone())
            .unwrap_or_default(),
        criteria: selected
            .map(|candidate| candidate.promotion.criteria.clone())
            .unwrap_or_default(),
        estimated_time_token_savings: selected
            .map(|candidate| candidate.promotion.estimated_time_token_savings.clone())
            .unwrap_or_default(),
    };

    let redaction = BundleRedactionSummary {
        applied: !fixture_payloads.is_empty(),
        rules: vec![
            "sensitive_keys".to_string(),
            "secret_value_heuristic".to_string(),
        ],
        summary: if fixture_payloads.is_empty() {
            "no fixtures emitted".to_string()
        } else {
            "fixture payloads scrubbed of secret-like values and sensitive keys before write"
                .to_string()
        },
        fixture_count: fixture_payloads.len(),
    };

    let eval_pack = if eval_pack_toml.trim().is_empty() {
        None
    } else {
        Some(BundleEvalPackRef {
            path: BUNDLE_EVAL_PACK_FILE.to_string(),
            link: selected
                .and_then(|candidate| candidate.promotion.eval_pack_link.clone())
                .filter(|link| !link.trim().is_empty()),
        })
    };

    let manifest = CrystallizationBundleManifest {
        schema: BUNDLE_SCHEMA.to_string(),
        schema_version: BUNDLE_SCHEMA_VERSION,
        generated_at: now_rfc3339(),
        generator: BundleGenerator::default(),
        kind,
        candidate_id: selected
            .map(|candidate| candidate.id.clone())
            .unwrap_or_default(),
        external_key,
        title,
        team: options.team,
        repo: options.repo,
        risk_level,
        workflow: manifest_workflow,
        source_trace_hashes,
        source_traces,
        deterministic_steps,
        fuzzy_steps,
        side_effects: selected
            .map(|candidate| candidate.side_effects.clone())
            .unwrap_or_default(),
        capabilities: selected
            .map(|candidate| candidate.capabilities.clone())
            .unwrap_or_default(),
        required_secrets: selected
            .map(|candidate| candidate.required_secrets.clone())
            .unwrap_or_default(),
        savings: selected
            .map(|candidate| candidate.savings.clone())
            .unwrap_or_default(),
        shadow: selected
            .map(|candidate| candidate.shadow.clone())
            .unwrap_or_default(),
        eval_pack,
        fixtures: fixture_refs,
        promotion,
        redaction,
        confidence: selected
            .map(|candidate| candidate.confidence)
            .unwrap_or(0.0),
        rejection_reasons: report
            .rejected_candidates
            .iter()
            .flat_map(|candidate| candidate.rejection_reasons.iter().cloned())
            .collect(),
        warnings: report.warnings.clone(),
    };

    Ok(CrystallizationBundle {
        manifest,
        report,
        harn_code,
        eval_pack_toml,
        fixtures: fixture_payloads,
    })
}

/// Write a bundle to a directory. Creates the directory if it does not
/// already exist. Returns the manifest with `generated_at` and any
/// runtime-resolved metadata filled in.
pub fn write_crystallization_bundle(
    bundle: &CrystallizationBundle,
    bundle_dir: &Path,
) -> Result<CrystallizationBundleManifest, VmError> {
    std::fs::create_dir_all(bundle_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create bundle dir {}: {error}",
            bundle_dir.display()
        ))
    })?;
    write_bytes(
        &bundle_dir.join(BUNDLE_WORKFLOW_FILE),
        bundle.harn_code.as_bytes(),
    )?;
    let report_json = serde_json::to_vec_pretty(&bundle.report)
        .map_err(|error| VmError::Runtime(format!("failed to encode report JSON: {error}")))?;
    write_bytes(&bundle_dir.join(BUNDLE_REPORT_FILE), &report_json)?;

    if !bundle.eval_pack_toml.trim().is_empty() {
        write_bytes(
            &bundle_dir.join(BUNDLE_EVAL_PACK_FILE),
            bundle.eval_pack_toml.as_bytes(),
        )?;
    }

    if !bundle.fixtures.is_empty() {
        let fixtures_dir = bundle_dir.join(BUNDLE_FIXTURES_DIR);
        std::fs::create_dir_all(&fixtures_dir).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create fixtures dir {}: {error}",
                fixtures_dir.display()
            ))
        })?;
        for trace in &bundle.fixtures {
            let path = fixtures_dir.join(format!("{}.json", sanitize_fixture_name(&trace.id)));
            let payload = serde_json::to_vec_pretty(trace).map_err(|error| {
                VmError::Runtime(format!("failed to encode fixture {}: {error}", trace.id))
            })?;
            write_bytes(&path, &payload)?;
        }
    }

    let manifest_json = serde_json::to_vec_pretty(&bundle.manifest)
        .map_err(|error| VmError::Runtime(format!("failed to encode manifest JSON: {error}")))?;
    write_bytes(&bundle_dir.join(BUNDLE_MANIFEST_FILE), &manifest_json)?;
    Ok(bundle.manifest.clone())
}

/// Read a bundle manifest from disk. Verifies the schema marker but does
/// not cross-check workflow/report/eval-pack sibling files; for a richer
/// check use [`validate_crystallization_bundle`].
pub fn load_crystallization_bundle_manifest(
    bundle_dir: &Path,
) -> Result<CrystallizationBundleManifest, VmError> {
    let manifest_path = bundle_dir.join(BUNDLE_MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read bundle manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: CrystallizationBundleManifest =
        serde_json::from_slice(&bytes).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode bundle manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
    if manifest.schema != BUNDLE_SCHEMA {
        return Err(VmError::Runtime(format!(
            "bundle {} has unrecognized schema {:?} (expected {})",
            bundle_dir.display(),
            manifest.schema,
            BUNDLE_SCHEMA
        )));
    }
    if manifest.schema_version > BUNDLE_SCHEMA_VERSION {
        return Err(VmError::Runtime(format!(
            "bundle {} schema_version {} is newer than supported {}",
            bundle_dir.display(),
            manifest.schema_version,
            BUNDLE_SCHEMA_VERSION
        )));
    }
    Ok(manifest)
}

/// Read every fixture trace referenced by the bundle manifest. Returns
/// the manifest plus loaded traces, in the order they appear in the
/// manifest. Fixtures with `path: None` are skipped.
pub fn load_crystallization_bundle(
    bundle_dir: &Path,
) -> Result<(CrystallizationBundleManifest, Vec<CrystallizationTrace>), VmError> {
    let manifest = load_crystallization_bundle_manifest(bundle_dir)?;
    let mut traces = Vec::new();
    for fixture in &manifest.fixtures {
        let path = bundle_dir.join(&fixture.path);
        traces.push(load_crystallization_trace(&path)?);
    }
    Ok((manifest, traces))
}

/// Validate a bundle directory layout and contents. Cheap enough to call
/// from a CLI smoke command; performs no live side effects.
pub fn validate_crystallization_bundle(bundle_dir: &Path) -> Result<BundleValidation, VmError> {
    let mut validation = BundleValidation {
        bundle_dir: bundle_dir.display().to_string(),
        ..BundleValidation::default()
    };
    let manifest = match load_crystallization_bundle_manifest(bundle_dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            validation.problems.push(error.to_string());
            return Ok(validation);
        }
    };
    validation.manifest_ok = true;
    validation.schema = manifest.schema.clone();
    validation.schema_version = manifest.schema_version;
    validation.kind = manifest.kind.clone();
    validation.candidate_id = manifest.candidate_id.clone();

    let workflow_path = bundle_dir.join(&manifest.workflow.path);
    if workflow_path.exists() {
        validation.workflow_ok = true;
    } else {
        validation
            .problems
            .push(format!("missing workflow file {}", workflow_path.display()));
    }

    let report_path = bundle_dir.join(BUNDLE_REPORT_FILE);
    match std::fs::read(&report_path) {
        Ok(bytes) => match serde_json::from_slice::<CrystallizationReport>(&bytes) {
            Ok(report) => {
                validation.report_ok = true;
                if matches!(manifest.kind, BundleKind::Candidate | BundleKind::PlanOnly)
                    && manifest.candidate_id.is_empty()
                {
                    validation
                        .problems
                        .push("manifest is non-rejected but has empty candidate_id".to_string());
                }
                if matches!(manifest.kind, BundleKind::Candidate | BundleKind::PlanOnly)
                    && report.selected_candidate_id.as_deref() != Some(&manifest.candidate_id)
                {
                    validation.problems.push(format!(
                        "report selected_candidate_id {:?} does not match manifest candidate_id {}",
                        report.selected_candidate_id, manifest.candidate_id
                    ));
                }
            }
            Err(error) => {
                validation
                    .problems
                    .push(format!("invalid report.json: {error}"));
            }
        },
        Err(error) => {
            validation.problems.push(format!(
                "missing report file {}: {error}",
                report_path.display()
            ));
        }
    }

    if let Some(eval_pack) = &manifest.eval_pack {
        let path = bundle_dir.join(&eval_pack.path);
        if path.exists() {
            validation.eval_pack_ok = true;
        } else {
            validation.problems.push(format!(
                "manifest references eval pack {} but file is missing",
                path.display()
            ));
        }
    } else {
        validation.eval_pack_ok = true;
    }

    let mut fixtures_problem = false;
    for fixture in &manifest.fixtures {
        let path = bundle_dir.join(&fixture.path);
        if !path.exists() {
            validation
                .problems
                .push(format!("missing fixture {}", path.display()));
            fixtures_problem = true;
            continue;
        }
        if !fixture.redacted {
            validation.problems.push(format!(
                "fixture {} is not marked redacted; bundle must not ship raw private payloads",
                fixture.path
            ));
            fixtures_problem = true;
        }
    }
    validation.fixtures_ok = !fixtures_problem;

    if !manifest.redaction.applied && !manifest.fixtures.is_empty() {
        validation
            .problems
            .push("redaction.applied is false but bundle includes fixtures".to_string());
    } else {
        validation.redaction_ok = true;
    }
    if !manifest
        .required_secrets
        .iter()
        .all(|secret| secret_id_looks_logical(secret))
    {
        validation.problems.push(
            "required_secrets contains a non-logical id (looks like a raw secret)".to_string(),
        );
    }

    Ok(validation)
}

/// Replay shadow comparison from a bundle: re-runs the deterministic
/// shadow check in-process against the bundle's redacted fixtures, with
/// no live side effects. Returns the manifest and the freshly computed
/// `ShadowRunReport`. The returned report is suitable for cloud import or
/// for asserting determinism in CI.
pub fn shadow_replay_bundle(
    bundle_dir: &Path,
) -> Result<(CrystallizationBundleManifest, ShadowRunReport), VmError> {
    let (manifest, traces) = load_crystallization_bundle(bundle_dir)?;
    let report_path = bundle_dir.join(BUNDLE_REPORT_FILE);
    let bytes = std::fs::read(&report_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read bundle report {}: {error}",
            report_path.display()
        ))
    })?;
    let report: CrystallizationReport = serde_json::from_slice(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to decode bundle report {}: {error}",
            report_path.display()
        ))
    })?;
    let candidate = report
        .selected_candidate_id
        .as_deref()
        .and_then(|id| report.candidates.iter().find(|c| c.id == id))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "bundle {} has no selected candidate to replay",
                bundle_dir.display()
            ))
        })?;
    let shadow = shadow_candidate(candidate, &traces);
    Ok((manifest, shadow))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    crate::atomic_io::atomic_write(path, bytes)
        .map_err(|error| VmError::Runtime(format!("failed to write {}: {error}", path.display())))
}

fn sanitize_fixture_name(raw: &str) -> String {
    let cleaned = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.trim_matches('_').is_empty() {
        "trace".to_string()
    } else {
        cleaned.trim_matches('_').to_string()
    }
}

fn sanitize_external_key(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            out.push(lowered);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "crystallized-workflow".to_string()
    } else {
        trimmed
    }
}

fn infer_bundle_title(candidate: Option<&WorkflowCandidate>, fallback_name: &str) -> String {
    if let Some(candidate) = candidate {
        format!(
            "{} ({} step{})",
            candidate.name,
            candidate.steps.len(),
            if candidate.steps.len() == 1 { "" } else { "s" }
        )
    } else {
        format!("rejected: {fallback_name}")
    }
}

fn infer_risk_level(candidate: Option<&WorkflowCandidate>) -> String {
    let Some(candidate) = candidate else {
        return "high".to_string();
    };
    let touches_external = candidate.side_effects.iter().any(side_effect_is_external);
    let needs_secret = !candidate.required_secrets.is_empty();
    if touches_external && needs_secret {
        "high".to_string()
    } else if touches_external || needs_secret {
        "medium".to_string()
    } else {
        "low".to_string()
    }
}

fn side_effect_is_external(effect: &CrystallizationSideEffect) -> bool {
    let kind = effect.kind.to_ascii_lowercase();
    if kind.is_empty() {
        return false;
    }
    // Plan-only side effects stay inside Harn's own data plane: they
    // write receipts, append to the in-process event log, or stash plans.
    // None of those touch tenant-external systems.
    let internal = kind.contains("receipt")
        || kind.contains("event_log")
        || kind.contains("memo")
        || kind.contains("plan");
    if internal {
        return false;
    }
    kind.contains("post")
        || kind.contains("write")
        || kind.contains("publish")
        || kind.contains("delete")
        || kind.contains("send")
}

fn candidate_is_plan_only(candidate: &WorkflowCandidate) -> bool {
    if candidate.steps.is_empty() {
        return false;
    }
    candidate.side_effects.iter().all(|effect| {
        let kind = effect.kind.to_ascii_lowercase();
        // Plan-only side effects stay inside Harn's own data plane: receipt
        // writes, in-memory event-log appends, file-only mutations, etc.
        kind.is_empty()
            || kind.contains("receipt")
            || kind.contains("event_log")
            || kind.contains("memo")
            || kind.contains("plan")
            || (kind.contains("file") && !kind.contains("publish"))
    })
}

fn redact_trace_for_bundle(trace: &mut CrystallizationTrace) {
    for action in &mut trace.actions {
        redact_bundle_value(&mut action.inputs);
        if let Some(output) = action.output.as_mut() {
            redact_bundle_value(output);
        }
        if let Some(observed) = action.observed_output.as_mut() {
            redact_bundle_value(observed);
        }
        for value in action.parameters.values_mut() {
            redact_bundle_value(value);
        }
        for (_, value) in action.metadata.iter_mut() {
            redact_bundle_value(value);
        }
    }
    for (_, value) in trace.metadata.iter_mut() {
        redact_bundle_value(value);
    }
    if let Some(run) = trace.replay_run.as_mut() {
        redact_replay_run_for_bundle(run);
    }
}

fn redact_replay_run_for_bundle(run: &mut ReplayTraceRun) {
    for value in run
        .event_log_entries
        .iter_mut()
        .chain(run.trigger_firings.iter_mut())
        .chain(run.llm_interactions.iter_mut())
        .chain(run.protocol_interactions.iter_mut())
        .chain(run.approval_interactions.iter_mut())
        .chain(run.effect_receipts.iter_mut())
        .chain(run.agent_transcript_deltas.iter_mut())
        .chain(run.final_artifacts.iter_mut())
        .chain(run.policy_decisions.iter_mut())
    {
        redact_bundle_value(value);
    }
}

fn redact_bundle_value(value: &mut JsonValue) {
    match value {
        JsonValue::String(text) if looks_like_secret_value(text) => {
            *text = "[redacted]".to_string();
        }
        JsonValue::Array(items) => {
            for item in items {
                redact_bundle_value(item);
            }
        }
        JsonValue::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_bundle_key(key) {
                    *child = JsonValue::String("[redacted]".to_string());
                } else {
                    redact_bundle_value(child);
                }
            }
        }
        _ => {}
    }
}

fn is_sensitive_bundle_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "authorization"
        || lower == "cookie"
        || lower == "set-cookie"
}

fn looks_like_secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("ghs_")
        || trimmed.starts_with("xoxb-")
        || trimmed.starts_with("xoxp-")
        || trimmed.starts_with("AKIA")
        || (trimmed.len() > 48
            && trimmed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
}

fn secret_id_looks_logical(value: &str) -> bool {
    !looks_like_secret_value(value) && !value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version_trace(
        id: &str,
        version: &str,
        side_target: &str,
        fuzzy: bool,
    ) -> CrystallizationTrace {
        CrystallizationTrace {
            id: id.to_string(),
            actions: vec![
                CrystallizationAction {
                    id: format!("{id}-branch"),
                    kind: "tool_call".to_string(),
                    name: "git.checkout_branch".to_string(),
                    parameters: BTreeMap::from([
                        ("repo_path".to_string(), json!(format!("/tmp/{id}"))),
                        (
                            "branch_name".to_string(),
                            json!(format!("release-{version}")),
                        ),
                    ]),
                    side_effects: vec![CrystallizationSideEffect {
                        kind: "git_ref".to_string(),
                        target: side_target.to_string(),
                        capability: Some("git.write".to_string()),
                        ..CrystallizationSideEffect::default()
                    }],
                    capabilities: vec!["git.write".to_string()],
                    deterministic: Some(true),
                    duration_ms: Some(20),
                    ..CrystallizationAction::default()
                },
                CrystallizationAction {
                    id: format!("{id}-manifest"),
                    kind: "file_mutation".to_string(),
                    name: "update_manifest_version".to_string(),
                    inputs: json!({"version": version, "path": "harn.toml"}),
                    parameters: BTreeMap::from([("version".to_string(), json!(version))]),
                    side_effects: vec![CrystallizationSideEffect {
                        kind: "file_write".to_string(),
                        target: "harn.toml".to_string(),
                        capability: Some("fs.write".to_string()),
                        ..CrystallizationSideEffect::default()
                    }],
                    capabilities: vec!["fs.write".to_string()],
                    deterministic: Some(true),
                    ..CrystallizationAction::default()
                },
                CrystallizationAction {
                    id: format!("{id}-release"),
                    kind: if fuzzy { "model_call" } else { "tool_call" }.to_string(),
                    name: "prepare_release_notes".to_string(),
                    inputs: json!({"release_target": "crates.io", "version": version}),
                    parameters: BTreeMap::from([
                        ("release_target".to_string(), json!("crates.io")),
                        ("version".to_string(), json!(version)),
                    ]),
                    fuzzy: Some(fuzzy),
                    deterministic: Some(!fuzzy),
                    cost: CrystallizationCost {
                        model_calls: if fuzzy { 1 } else { 0 },
                        input_tokens: if fuzzy { 1200 } else { 0 },
                        output_tokens: if fuzzy { 250 } else { 0 },
                        total_cost_usd: if fuzzy { 0.01 } else { 0.0 },
                        wall_ms: 3000,
                        ..CrystallizationCost::default()
                    },
                    ..CrystallizationAction::default()
                },
            ],
            ..CrystallizationTrace::default()
        }
    }

    #[test]
    fn crystallizes_repeated_version_bump_with_parameters() {
        let traces = (0..5)
            .map(|idx| {
                version_trace(
                    &format!("trace_{idx}"),
                    &format!("0.7.{idx}"),
                    "release-branch",
                    false,
                )
            })
            .collect::<Vec<_>>();

        let artifacts = crystallize_traces(
            traces,
            CrystallizeOptions {
                workflow_name: Some("version_bump".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .unwrap();

        let candidate = &artifacts.report.candidates[0];
        assert!(candidate.rejection_reasons.is_empty());
        assert!(candidate.shadow.pass);
        assert_eq!(candidate.examples.len(), 5);
        let params = candidate
            .parameters
            .iter()
            .map(|param| param.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(params.contains("version"));
        assert!(params.contains("repo_path"));
        assert!(params.contains("branch_name"));
        assert!(artifacts.harn_code.contains("pipeline version_bump("));
        assert!(artifacts.eval_pack_toml.contains("crystallization-shadow"));
    }

    #[test]
    fn rejects_divergent_side_effects() {
        let traces = vec![
            version_trace("trace_a", "0.7.1", "release-branch", false),
            version_trace("trace_b", "0.7.2", "main", false),
            version_trace("trace_c", "0.7.3", "release-branch", false),
        ];

        let artifacts = crystallize_traces(traces, CrystallizeOptions::default()).unwrap();

        assert!(artifacts.report.candidates.is_empty());
        assert_eq!(artifacts.report.rejected_candidates.len(), 1);
        assert!(artifacts.report.rejected_candidates[0].rejection_reasons[0]
            .contains("divergent side effects"));
    }

    #[test]
    fn preserves_remaining_fuzzy_segment() {
        let traces = (0..3)
            .map(|idx| {
                version_trace(
                    &format!("trace_{idx}"),
                    &format!("0.8.{idx}"),
                    "release-branch",
                    true,
                )
            })
            .collect::<Vec<_>>();

        let artifacts = crystallize_traces(traces, CrystallizeOptions::default()).unwrap();
        let candidate = &artifacts.report.candidates[0];

        assert!(candidate
            .steps
            .iter()
            .any(|step| step.segment == SegmentKind::Fuzzy));
        assert!(candidate.savings.remaining_model_calls > 0);
        assert!(artifacts.harn_code.contains("TODO: fuzzy segment"));
    }

    #[test]
    fn excludes_unresolved_policy_violations_from_mining() {
        let mut traces = version_traces(2);
        let mut excluded = version_trace("trace_policy_blocked", "0.7.99", "release-branch", false);
        excluded
            .metadata
            .insert("unresolved_policy_violation".to_string(), json!(true));
        traces.push(excluded);

        let artifacts = crystallize_traces(
            traces,
            CrystallizeOptions {
                min_examples: 2,
                ..CrystallizeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(artifacts.report.source_trace_count, 2);
        assert_eq!(artifacts.report.excluded_trace_count, 1);
        assert!(artifacts.report.selected_candidate_id.is_some());
    }

    fn plan_only_trace(id: &str, suffix: &str) -> CrystallizationTrace {
        CrystallizationTrace {
            id: id.to_string(),
            actions: vec![
                CrystallizationAction {
                    id: format!("{id}-classify"),
                    kind: "tool_call".to_string(),
                    name: "classify_issue".to_string(),
                    parameters: BTreeMap::from([
                        ("issue_id".to_string(), json!(format!("HAR-{suffix}"))),
                        ("team_key".to_string(), json!("HAR")),
                    ]),
                    capabilities: vec!["linear.read".to_string()],
                    deterministic: Some(true),
                    duration_ms: Some(15),
                    ..CrystallizationAction::default()
                },
                CrystallizationAction {
                    id: format!("{id}-receipt"),
                    kind: "receipt_write".to_string(),
                    name: "emit_receipt".to_string(),
                    inputs: json!({"summary": format!("plan only #{suffix}"), "kind": "plan"}),
                    parameters: BTreeMap::from([
                        ("kind".to_string(), json!("plan")),
                        ("summary".to_string(), json!(format!("plan only #{suffix}"))),
                    ]),
                    side_effects: vec![CrystallizationSideEffect {
                        kind: "receipt_write".to_string(),
                        target: "tenant_event_log".to_string(),
                        capability: Some("receipt.write".to_string()),
                        ..CrystallizationSideEffect::default()
                    }],
                    capabilities: vec!["receipt.write".to_string()],
                    deterministic: Some(true),
                    duration_ms: Some(5),
                    ..CrystallizationAction::default()
                },
            ],
            ..CrystallizationTrace::default()
        }
    }

    fn version_traces(count: usize) -> Vec<CrystallizationTrace> {
        (0..count)
            .map(|idx| {
                version_trace(
                    &format!("trace_{idx}"),
                    &format!("0.7.{idx}"),
                    "release-branch",
                    false,
                )
            })
            .collect()
    }

    #[test]
    fn build_bundle_assembles_versioned_manifest() {
        let traces = version_traces(5);
        let artifacts = crystallize_traces(
            traces.clone(),
            CrystallizeOptions {
                workflow_name: Some("version_bump".to_string()),
                package_name: Some("release-workflows".to_string()),
                author: Some("ops@example.com".to_string()),
                approver: Some("lead@example.com".to_string()),
                eval_pack_link: Some("eval-pack://release-workflows/v1".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .unwrap();

        let bundle = build_crystallization_bundle(
            artifacts,
            &traces,
            BundleOptions {
                team: Some("platform".to_string()),
                repo: Some("burin-labs/harn".to_string()),
                ..BundleOptions::default()
            },
        )
        .unwrap();

        let manifest = &bundle.manifest;
        assert_eq!(manifest.schema, BUNDLE_SCHEMA);
        assert_eq!(manifest.schema_version, BUNDLE_SCHEMA_VERSION);
        assert_eq!(manifest.kind, BundleKind::Candidate);
        assert!(!manifest.candidate_id.is_empty());
        assert_eq!(manifest.workflow.name, "version_bump");
        assert_eq!(manifest.workflow.package_name, "release-workflows");
        assert_eq!(manifest.workflow.path, BUNDLE_WORKFLOW_FILE);
        assert_eq!(manifest.team.as_deref(), Some("platform"));
        assert_eq!(manifest.repo.as_deref(), Some("burin-labs/harn"));
        assert_eq!(manifest.external_key, "version-bump");
        assert_eq!(manifest.promotion.rollout_policy, "shadow_then_canary");
        assert_eq!(
            manifest.promotion.author.as_deref(),
            Some("ops@example.com")
        );
        assert_eq!(
            manifest.promotion.approver.as_deref(),
            Some("lead@example.com")
        );
        assert_eq!(manifest.promotion.workflow_version, "0.1.0");
        assert!(manifest.deterministic_steps.len() + manifest.fuzzy_steps.len() > 0);
        assert_eq!(manifest.source_traces.len(), traces.len());
        assert_eq!(manifest.fixtures.len(), traces.len());
        assert!(manifest.fixtures.iter().all(|fixture| fixture.redacted));
        assert!(manifest.redaction.applied);
        assert!(manifest.redaction.fixture_count > 0);
        assert!(manifest
            .eval_pack
            .as_ref()
            .is_some_and(|eval| eval.path == BUNDLE_EVAL_PACK_FILE));
        assert!(manifest
            .required_secrets
            .iter()
            .all(|secret| !secret.is_empty()));
    }

    #[test]
    fn write_bundle_round_trips_through_disk() {
        let traces = version_traces(5);
        let artifacts = crystallize_traces(
            traces.clone(),
            CrystallizeOptions {
                workflow_name: Some("version_bump".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let written = write_crystallization_bundle(&bundle, dir.path()).unwrap();
        assert_eq!(written.candidate_id, bundle.manifest.candidate_id);

        // Files exist on disk.
        for relative in [
            BUNDLE_MANIFEST_FILE,
            BUNDLE_REPORT_FILE,
            BUNDLE_WORKFLOW_FILE,
            BUNDLE_EVAL_PACK_FILE,
        ] {
            assert!(dir.path().join(relative).exists(), "missing {relative}");
        }
        let fixtures_dir = dir.path().join(BUNDLE_FIXTURES_DIR);
        assert!(fixtures_dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&fixtures_dir).unwrap().count(),
            traces.len()
        );

        // Manifest round-trips.
        let (loaded_manifest, loaded_traces) = load_crystallization_bundle(dir.path()).unwrap();
        assert_eq!(loaded_manifest, bundle.manifest);
        assert_eq!(loaded_traces.len(), traces.len());

        // Validation passes.
        let validation = validate_crystallization_bundle(dir.path()).unwrap();
        assert!(
            validation.problems.is_empty(),
            "unexpected problems: {:?}",
            validation.problems
        );
        assert!(validation.is_ok());
        assert!(validation.workflow_ok && validation.report_ok);
        assert!(validation.fixtures_ok && validation.redaction_ok);

        // Shadow replay matches the persisted shadow report.
        let (replay_manifest, shadow) = shadow_replay_bundle(dir.path()).unwrap();
        assert_eq!(replay_manifest.candidate_id, bundle.manifest.candidate_id);
        assert!(shadow.pass, "shadow should still pass");
        assert_eq!(shadow.compared_traces, traces.len());
    }

    #[test]
    fn validate_rejects_bundle_with_missing_workflow() {
        let traces = version_traces(3);
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();

        let dir = tempfile::tempdir().unwrap();
        write_crystallization_bundle(&bundle, dir.path()).unwrap();
        std::fs::remove_file(dir.path().join(BUNDLE_WORKFLOW_FILE)).unwrap();

        let validation = validate_crystallization_bundle(dir.path()).unwrap();
        assert!(!validation.is_ok());
        assert!(validation
            .problems
            .iter()
            .any(|problem| problem.contains("missing workflow file")));
    }

    #[test]
    fn validate_rejects_bundle_with_unredacted_fixture() {
        let traces = version_traces(3);
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let mut bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        // Force a fixture to claim it is unredacted; this must trip
        // validation so a malicious or careless producer cannot ship raw
        // private payloads under the bundle contract.
        bundle.manifest.fixtures[0].redacted = false;
        let dir = tempfile::tempdir().unwrap();
        write_crystallization_bundle(&bundle, dir.path()).unwrap();

        let validation = validate_crystallization_bundle(dir.path()).unwrap();
        assert!(!validation.is_ok());
        assert!(validation
            .problems
            .iter()
            .any(|problem| problem.contains("not marked redacted")));
    }

    #[test]
    fn validate_rejects_unsupported_schema_version() {
        let traces = version_traces(3);
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let mut bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        bundle.manifest.schema_version = BUNDLE_SCHEMA_VERSION + 1;
        let dir = tempfile::tempdir().unwrap();
        write_crystallization_bundle(&bundle, dir.path()).unwrap();

        let validation = validate_crystallization_bundle(dir.path()).unwrap();
        assert!(!validation.is_ok());
        assert!(validation
            .problems
            .iter()
            .any(|problem| problem.contains("schema_version")));
    }

    #[test]
    fn redacts_secret_like_values_in_fixtures() {
        // Build secret-shaped strings at runtime so we exercise the
        // redaction prefixes (`xoxb-`, `ghp_`, `sk-`) without checking in
        // source that looks like a real credential to secret scanners.
        let slack_prefix = format!("{}{}", "xo", "xb-");
        let github_prefix = format!("{}{}", "gh", "p_");
        let openai_prefix = "sk-".to_string();
        let pad = "A".repeat(48);
        let slack_secret = format!("{slack_prefix}1234567890-{pad}");
        let github_secret = format!("{github_prefix}{pad}");
        let openai_secret = format!("{openai_prefix}{pad}");

        let mut secret_action = CrystallizationAction {
            id: "secret".to_string(),
            kind: "tool_call".to_string(),
            name: "post_release_to_slack".to_string(),
            parameters: BTreeMap::from([
                ("slack_token".to_string(), json!(slack_secret)),
                ("channel".to_string(), json!("#releases")),
            ]),
            inputs: json!({
                "authorization": format!("Bearer {github_secret}"),
                "version": "0.7.1",
            }),
            ..CrystallizationAction::default()
        };
        secret_action
            .metadata
            .insert("api_key".to_string(), json!(openai_secret));

        let mut trace = CrystallizationTrace {
            id: "trace_secret".to_string(),
            actions: vec![secret_action],
            ..CrystallizationTrace::default()
        };
        redact_trace_for_bundle(&mut trace);
        let action = &trace.actions[0];
        assert_eq!(
            action.parameters.get("slack_token"),
            Some(&json!("[redacted]"))
        );
        assert_eq!(action.parameters.get("channel"), Some(&json!("#releases")));
        let inputs = action.inputs.as_object().unwrap();
        assert_eq!(inputs.get("authorization"), Some(&json!("[redacted]")));
        assert_eq!(inputs.get("version"), Some(&json!("0.7.1")));
        assert_eq!(action.metadata.get("api_key"), Some(&json!("[redacted]")));
    }

    #[test]
    fn plan_only_fixture_yields_plan_only_kind() {
        let traces = (0..3)
            .map(|idx| plan_only_trace(&format!("plan_{idx}"), &format!("{idx}")))
            .collect::<Vec<_>>();
        let artifacts = crystallize_traces(
            traces.clone(),
            CrystallizeOptions {
                workflow_name: Some("plan_only_triage".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        assert_eq!(bundle.manifest.kind, BundleKind::PlanOnly);
        assert_eq!(bundle.manifest.risk_level, "low");
    }

    #[test]
    fn rejected_bundle_has_rejected_kind() {
        let traces = vec![
            version_trace("trace_a", "0.7.1", "release-branch", false),
            version_trace("trace_b", "0.7.2", "main", false),
            version_trace("trace_c", "0.7.3", "release-branch", false),
        ];
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        assert_eq!(bundle.manifest.kind, BundleKind::Rejected);
        assert!(bundle.manifest.candidate_id.is_empty());
        assert!(!bundle.manifest.rejection_reasons.is_empty());
        assert!(bundle.fixtures.is_empty());
    }

    #[test]
    fn validate_round_trips_rejected_bundle() {
        let traces = vec![
            version_trace("trace_a", "0.7.1", "release-branch", false),
            version_trace("trace_b", "0.7.2", "main", false),
            version_trace("trace_c", "0.7.3", "release-branch", false),
        ];
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_crystallization_bundle(&bundle, dir.path()).unwrap();

        let validation = validate_crystallization_bundle(dir.path()).unwrap();
        assert!(validation.is_ok(), "{:?}", validation.problems);
        assert_eq!(validation.kind, BundleKind::Rejected);
    }

    #[test]
    fn shadow_replay_fails_when_fixture_diverges() {
        let traces = version_traces(3);
        let artifacts = crystallize_traces(traces.clone(), CrystallizeOptions::default()).unwrap();
        let bundle =
            build_crystallization_bundle(artifacts, &traces, BundleOptions::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_crystallization_bundle(&bundle, dir.path()).unwrap();

        // Tamper with one redacted fixture so its action list no longer
        // matches the candidate signature; the replay must fail without
        // panicking.
        let fixture_dir = dir.path().join(BUNDLE_FIXTURES_DIR);
        let some_fixture = std::fs::read_dir(&fixture_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut tampered: CrystallizationTrace =
            serde_json::from_slice(&std::fs::read(&some_fixture).unwrap()).unwrap();
        tampered.actions.truncate(1);
        std::fs::write(&some_fixture, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();

        let (_, shadow) = shadow_replay_bundle(dir.path()).unwrap();
        assert!(!shadow.pass);
        assert!(!shadow.failures.is_empty());
    }
}
