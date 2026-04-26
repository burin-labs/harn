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

use super::{new_id, now_rfc3339, RunRecord};
use crate::value::VmError;

const TRACE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MIN_EXAMPLES: usize = 2;

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
    pub pass: bool,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShadowRunReport {
    pub pass: bool,
    pub compared_traces: usize,
    pub failures: Vec<String>,
    pub traces: Vec<ShadowTraceResult>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowCandidate {
    pub id: String,
    pub name: String,
    pub confidence: f64,
    pub sequence_signature: Vec<String>,
    pub parameters: Vec<WorkflowCandidateParameter>,
    pub steps: Vec<WorkflowCandidateStep>,
    pub examples: Vec<WorkflowCandidateExample>,
    pub capabilities: Vec<String>,
    pub required_secrets: Vec<String>,
    pub approval_points: Vec<CrystallizationApproval>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub expected_outputs: Vec<JsonValue>,
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
    pub selected_candidate_id: Option<String>,
    pub candidates: Vec<WorkflowCandidate>,
    pub rejected_candidates: Vec<WorkflowCandidate>,
    pub warnings: Vec<String>,
    pub input_format: CrystallizationInputFormat,
    pub harn_code_path: Option<String>,
    pub eval_pack_path: Option<String>,
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

    let normalized = traces.into_iter().map(normalize_trace).collect::<Vec<_>>();
    let mut candidates = mine_candidates(&normalized, min_examples, &options);
    let mut rejected_candidates = Vec::new();
    for candidate in &mut candidates {
        candidate.shadow = shadow_candidate(candidate, &normalized);
        if !candidate.shadow.pass {
            candidate
                .rejection_reasons
                .extend(candidate.shadow.failures.clone());
        }
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
    };

    Ok(CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    })
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
    if let Some(parent) = workflow_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create workflow output dir {}: {error}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(workflow_path, &artifacts.harn_code).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write generated workflow {}: {error}",
            workflow_path.display()
        ))
    })?;

    artifacts.report.harn_code_path = Some(workflow_path.display().to_string());
    if let Some(path) = eval_pack_path {
        if !artifacts.eval_pack_toml.trim().is_empty() {
            if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|error| {
                    VmError::Runtime(format!(
                        "failed to create eval pack output dir {}: {error}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::write(path, &artifacts.eval_pack_toml).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to write eval pack {}: {error}",
                    path.display()
                ))
            })?;
            artifacts.report.eval_pack_path = Some(path.display().to_string());
            if let Some(candidate) = artifacts.report.candidates.first_mut() {
                candidate.promotion.eval_pack_link = Some(path.display().to_string());
            }
        }
    }

    if let Some(parent) = report_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create report output dir {}: {error}",
                parent.display()
            ))
        })?;
    }
    let report_json = serde_json::to_string_pretty(&artifacts.report)
        .map_err(|error| VmError::Runtime(format!("failed to encode report JSON: {error}")))?;
    std::fs::write(report_path, report_json).map_err(|error| {
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
        sequence_signature: sequence,
        parameters,
        steps,
        examples: example_refs.clone(),
        capabilities: capabilities.clone(),
        required_secrets: required_secrets.clone(),
        approval_points,
        side_effects,
        expected_outputs,
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

fn shadow_candidate(
    candidate: &WorkflowCandidate,
    traces: &[CrystallizationTrace],
) -> ShadowRunReport {
    let mut failures = Vec::new();
    let mut results = Vec::new();
    for example in &candidate.examples {
        let Some(trace) = traces.iter().find(|trace| trace.id == example.trace_id) else {
            failures.push(format!("missing source trace {}", example.trace_id));
            continue;
        };
        let mut details = Vec::new();
        let end = example.start_index + candidate.steps.len();
        if end > trace.actions.len() {
            details.push("candidate sequence extends past trace action list".to_string());
        } else {
            let signatures = trace.actions[example.start_index..end]
                .iter()
                .map(action_signature)
                .collect::<Vec<_>>();
            if signatures != candidate.sequence_signature {
                details.push("action sequence signature changed".to_string());
            }
            for (offset, step) in candidate.steps.iter().enumerate() {
                let action = &trace.actions[example.start_index + offset];
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
                            details
                                .push(format!("step {} deterministic output differs", step.index));
                        }
                    }
                }
            }
        }
        let pass = details.is_empty();
        if !pass {
            failures.push(format!("trace {} failed shadow comparison", trace.id));
        }
        results.push(ShadowTraceResult {
            trace_id: trace.id.clone(),
            pass,
            details,
        });
    }
    ShadowRunReport {
        pass: failures.is_empty(),
        compared_traces: results.len(),
        failures,
        traces: results,
    }
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
}
