//! Run records, replay fixtures, eval reports, and diff utilities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    default_run_dir, new_id, now_rfc3339, parse_json_payload, parse_json_value, ArtifactRecord,
    CapabilityPolicy,
};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};

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
    pub expected_status: String,
    pub stage_assertions: Vec<ReplayStageAssertion>,
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
    pub stage_id: Option<String>,
    pub node_id: Option<String>,
    pub worker_id: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
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
pub struct RunObservabilityRecord {
    pub schema_version: usize,
    pub planner_rounds: Vec<RunPlannerRoundRecord>,
    pub research_fact_count: usize,
    pub action_graph_nodes: Vec<RunActionGraphNodeRecord>,
    pub action_graph_edges: Vec<RunActionGraphEdgeRecord>,
    pub worker_lineage: Vec<RunWorkerLineageRecord>,
    pub verification_outcomes: Vec<RunVerificationOutcomeRecord>,
    pub transcript_pointers: Vec<RunTranscriptPointerRecord>,
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
    pub policy: CapabilityPolicy,
    pub execution: Option<RunExecutionRecord>,
    pub transcript: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub replay_fixture: Option<ReplayFixture>,
    pub observability: Option<RunObservabilityRecord>,
    pub trace_spans: Vec<RunTraceSpanRecord>,
    pub tool_recordings: Vec<ToolCallRecord>,
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

pub fn derive_run_observability(
    run: &RunRecord,
    persisted_path: Option<&Path>,
) -> RunObservabilityRecord {
    let mut action_graph_nodes = Vec::new();
    let mut action_graph_edges = Vec::new();
    let mut verification_outcomes = Vec::new();
    let mut planner_rounds = Vec::new();
    let mut transcript_pointers = Vec::new();
    let mut research_fact_count = 0usize;

    let root_node_id = format!("run:{}", run.id);
    action_graph_nodes.push(RunActionGraphNodeRecord {
        id: root_node_id.clone(),
        label: run
            .workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone()),
        kind: "run".to_string(),
        status: run.status.clone(),
        outcome: run.status.clone(),
        stage_id: None,
        node_id: None,
        worker_id: None,
        run_id: Some(run.id.clone()),
        run_path: run.persisted_path.clone(),
    });

    let stage_node_ids = run
        .stages
        .iter()
        .map(|stage| (stage.id.clone(), format!("stage:{}", stage.id)))
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
        action_graph_nodes.push(RunActionGraphNodeRecord {
            id: graph_node_id.clone(),
            label: stage.node_id.clone(),
            kind: "stage".to_string(),
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            stage_id: Some(stage.id.clone()),
            node_id: Some(stage.node_id.clone()),
            worker_id: stage
                .metadata
                .get("worker_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            run_id: None,
            run_path: None,
        });
        if !incoming_nodes.contains(&stage.node_id) {
            action_graph_edges.push(RunActionGraphEdgeRecord {
                from_id: root_node_id.clone(),
                to_id: graph_node_id.clone(),
                kind: "entry".to_string(),
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
            let tools_used = json_string_array(
                payload
                    .get("tools_used")
                    .or_else(|| trace.and_then(|trace| trace.get("tools_used"))),
            );
            let successful_tools = json_string_array(payload.get("successful_tools"));
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
                tools_used,
                successful_tools,
                ledger_done_rejections: json_usize(payload.get("ledger_done_rejections")),
                task_ledger,
                research_facts,
            };
            let has_agentic_detail = planner_round.iteration_count > 0
                || planner_round.llm_call_count > 0
                || planner_round.tool_execution_count > 0
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
            kind: "transition".to_string(),
            label: transition.branch.clone(),
        });
    }

    let worker_lineage = run
        .child_runs
        .iter()
        .map(|child| {
            let worker_node_id = format!("worker:{}", child.worker_id);
            action_graph_nodes.push(RunActionGraphNodeRecord {
                id: worker_node_id.clone(),
                label: child.worker_name.clone(),
                kind: "worker".to_string(),
                status: child.status.clone(),
                outcome: child.status.clone(),
                stage_id: child.parent_stage_id.clone(),
                node_id: None,
                worker_id: Some(child.worker_id.clone()),
                run_id: child.run_id.clone(),
                run_path: child.run_path.clone(),
            });
            if let Some(parent_stage_id) = child.parent_stage_id.as_ref() {
                if let Some(stage_node_id) = stage_node_ids.get(parent_stage_id) {
                    action_graph_edges.push(RunActionGraphEdgeRecord {
                        from_id: stage_node_id.clone(),
                        to_id: worker_node_id,
                        kind: "delegates".to_string(),
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
    }

    if let Some(path) = persisted_path {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !stem.is_empty() {
            let sidecar_path = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("{stem}-llm/llm_transcript.jsonl"));
            transcript_pointers.push(RunTranscriptPointerRecord {
                id: "run:llm_transcript".to_string(),
                label: "LLM transcript sidecar".to_string(),
                kind: "llm_jsonl".to_string(),
                location: "run sidecar".to_string(),
                path: Some(sidecar_path.to_string_lossy().into_owned()),
                available: sidecar_path.exists(),
            });
        }
    }

    RunObservabilityRecord {
        schema_version: 1,
        planner_rounds,
        research_fact_count,
        action_graph_nodes,
        action_graph_edges,
        worker_lineage,
        verification_outcomes,
        transcript_pointers,
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

fn load_replay_fixture(path: &Path) -> Result<ReplayFixture, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read replay fixture: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse replay fixture: {e}")))
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
    if materialized.replay_fixture.is_none() {
        materialized.replay_fixture = Some(replay_fixture_from_run(&materialized));
    }
    materialized.persisted_path = Some(path.to_string_lossy().into_owned());
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
    Ok(path.to_string_lossy().into_owned())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    let mut run: RunRecord = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))?;
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    run.persisted_path
        .get_or_insert_with(|| path.to_string_lossy().into_owned());
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
