use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalStats {
    pub(super) total_runs: usize,
    pub(super) completed_runs: usize,
    pub(super) active_runs: usize,
    pub(super) failed_runs: usize,
    pub(super) avg_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalTrustGraphResponse {
    pub(super) records: Vec<harn_vm::TrustRecord>,
    pub(super) groups: Option<Vec<harn_vm::TrustTraceGroup>>,
    pub(super) summary: Vec<harn_vm::TrustAgentSummary>,
    pub(super) chain: harn_vm::TrustChainReport,
    pub(super) topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalRunSummary {
    pub(super) path: String,
    pub(super) id: String,
    pub(super) workflow_name: String,
    pub(super) status: String,
    pub(super) last_stage_node_id: Option<String>,
    pub(super) failure_summary: Option<String>,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) duration_ms: Option<u64>,
    pub(super) stage_count: usize,
    pub(super) child_run_count: usize,
    pub(super) call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) models: Vec<String>,
    pub(super) updated_at_ms: u128,
    /// Skill names that were activated at any point during this run.
    /// Used by the portal list filter `skill=<name>`.
    pub(super) skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalInsight {
    pub(super) label: String,
    pub(super) value: String,
    pub(super) detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalCostSummary {
    pub(super) total_cost_usd: f64,
    pub(super) call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalCostTrendPoint {
    pub(super) date: String,
    pub(super) pipeline: String,
    pub(super) cost_usd: f64,
    pub(super) call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalProviderCostBreakdown {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) cost_usd: f64,
    pub(super) call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalCostReport {
    pub(super) summary: PortalCostSummary,
    pub(super) trend: Vec<PortalCostTrendPoint>,
    pub(super) provider_breakdown: Vec<PortalProviderCostBreakdown>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalStage {
    pub(super) id: String,
    pub(super) node_id: String,
    pub(super) kind: String,
    pub(super) status: String,
    pub(super) outcome: String,
    pub(super) branch: Option<String>,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) duration_ms: Option<u64>,
    pub(super) artifact_count: usize,
    pub(super) attempt_count: usize,
    pub(super) verification_summary: Option<String>,
    pub(super) debug: PortalStageDebug,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalStageDebug {
    pub(super) call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) consumed_artifact_ids: Vec<String>,
    pub(super) produced_artifact_ids: Vec<String>,
    pub(super) selected_artifact_ids: Vec<String>,
    pub(super) worker_id: Option<String>,
    pub(super) error: Option<String>,
    pub(super) model_policy: Option<String>,
    pub(super) auto_compact: Option<String>,
    pub(super) output_visibility: Option<String>,
    pub(super) context_policy: Option<String>,
    pub(super) retry_policy: Option<String>,
    pub(super) capability_policy: Option<String>,
    pub(super) input_contract: Option<String>,
    pub(super) output_contract: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) system_prompt: Option<String>,
    pub(super) rendered_context: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalSpan {
    pub(super) span_id: u64,
    pub(super) parent_id: Option<u64>,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) start_ms: u64,
    pub(super) duration_ms: u64,
    pub(super) end_ms: u64,
    pub(super) label: String,
    pub(super) lane: usize,
    pub(super) depth: usize,
    pub(super) metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalActivity {
    pub(super) label: String,
    pub(super) kind: String,
    pub(super) started_offset_ms: u64,
    pub(super) duration_ms: u64,
    pub(super) stage_node_id: Option<String>,
    pub(super) call_id: Option<String>,
    pub(super) summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalArtifact {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) title: String,
    pub(super) source: Option<String>,
    pub(super) stage: Option<String>,
    pub(super) estimated_tokens: Option<usize>,
    pub(super) lineage_count: usize,
    pub(super) preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalTransition {
    pub(super) from_node_id: Option<String>,
    pub(super) to_node_id: String,
    pub(super) branch: Option<String>,
    pub(super) consumed_count: usize,
    pub(super) produced_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalCheckpoint {
    pub(super) reason: String,
    pub(super) ready_count: usize,
    pub(super) completed_count: usize,
    pub(super) last_stage_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalExecutionSummary {
    pub(super) cwd: Option<String>,
    pub(super) repo_path: Option<String>,
    pub(super) worktree_path: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) adapter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalPolicySummary {
    pub(super) tools: Vec<String>,
    pub(super) capabilities: Vec<String>,
    pub(super) workspace_roots: Vec<String>,
    pub(super) side_effect_level: Option<String>,
    pub(super) recursion_limit: Option<usize>,
    pub(super) tool_arg_constraints: Vec<String>,
    pub(super) validation_valid: Option<bool>,
    pub(super) validation_errors: Vec<String>,
    pub(super) validation_warnings: Vec<String>,
    pub(super) reachable_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalReplayAssertion {
    pub(super) node_id: String,
    pub(super) expected_status: String,
    pub(super) expected_outcome: String,
    pub(super) expected_branch: Option<String>,
    pub(super) required_artifact_kinds: Vec<String>,
    pub(super) visible_text_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalReplaySummary {
    pub(super) fixture_id: String,
    pub(super) source_run_id: String,
    pub(super) created_at: String,
    pub(super) expected_status: String,
    pub(super) stage_assertions: Vec<PortalReplayAssertion>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalTranscriptMessage {
    pub(super) role: String,
    pub(super) content: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalTranscriptStep {
    pub(super) call_id: String,
    pub(super) span_id: Option<u64>,
    pub(super) iteration: usize,
    pub(super) call_index: usize,
    pub(super) model: String,
    pub(super) provider: Option<String>,
    pub(super) kept_messages: usize,
    pub(super) added_messages: usize,
    pub(super) total_messages: usize,
    pub(super) input_tokens: Option<i64>,
    pub(super) output_tokens: Option<i64>,
    pub(super) system_prompt: Option<String>,
    pub(super) added_context: Vec<PortalTranscriptMessage>,
    pub(super) response_text: Option<String>,
    pub(super) thinking: Option<String>,
    pub(super) tool_calls: Vec<String>,
    pub(super) summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalStorySection {
    pub(super) title: String,
    pub(super) scope: String,
    pub(super) role: String,
    pub(super) source: String,
    pub(super) text: String,
    pub(super) preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalChildRun {
    pub(super) worker_name: String,
    pub(super) status: String,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) run_id: Option<String>,
    pub(super) run_path: Option<String>,
    pub(super) task: String,
}

/// One skill activation/deactivation interval, pre-processed so the
/// portal can render a horizontal timeline without re-parsing transcript
/// events on the client. `activated_iteration` and `deactivated_iteration`
/// reference the 0-based agent-loop iteration when the skill came in /
/// went out; `deactivated_iteration: None` means the skill stayed active
/// through the end of the run.
#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalSkillTimelineEntry {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) activated_iteration: i64,
    pub(super) deactivated_iteration: Option<i64>,
    pub(super) score: Option<f64>,
    pub(super) reason: String,
    pub(super) allowed_tools: Vec<String>,
    pub(super) scope: String,
}

/// One rank of the matcher from a `skill_matched` event.
#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalSkillMatchEvent {
    pub(super) iteration: i64,
    pub(super) strategy: String,
    pub(super) reassess: bool,
    pub(super) working_files: Vec<String>,
    pub(super) candidates: Vec<PortalSkillMatchCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalSkillMatchCandidate {
    pub(super) name: String,
    pub(super) score: f64,
    pub(super) reason: String,
    pub(super) activated: bool,
}

/// One tool-search invocation extracted from transcript events. Each
/// entry pairs a `tool_search_query` with its matching
/// `tool_search_result` so the portal can render a waterfall of "which
/// tools entered context in which turn, via which query".
#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalToolLoadEvent {
    pub(super) query: String,
    pub(super) strategy: String,
    pub(super) mode: String,
    pub(super) tool_use_id: Option<String>,
    pub(super) promoted: Vec<String>,
    pub(super) references: Vec<String>,
    pub(super) iteration: Option<i64>,
    pub(super) scope: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalRunDetail {
    pub(super) summary: PortalRunSummary,
    pub(super) task: String,
    pub(super) workflow_id: String,
    pub(super) parent_run_id: Option<String>,
    pub(super) root_run_id: Option<String>,
    pub(super) policy_summary: PortalPolicySummary,
    pub(super) replay_summary: Option<PortalReplaySummary>,
    pub(super) execution: Option<harn_vm::orchestration::RunExecutionRecord>,
    pub(super) insights: Vec<PortalInsight>,
    pub(super) stages: Vec<PortalStage>,
    pub(super) spans: Vec<PortalSpan>,
    pub(super) activities: Vec<PortalActivity>,
    pub(super) transitions: Vec<PortalTransition>,
    pub(super) checkpoints: Vec<PortalCheckpoint>,
    pub(super) artifacts: Vec<PortalArtifact>,
    pub(super) execution_summary: Option<PortalExecutionSummary>,
    pub(super) transcript_steps: Vec<PortalTranscriptStep>,
    pub(super) story: Vec<PortalStorySection>,
    pub(super) child_runs: Vec<PortalChildRun>,
    pub(super) observability: harn_vm::orchestration::RunObservabilityRecord,
    pub(super) skill_timeline: Vec<PortalSkillTimelineEntry>,
    pub(super) skill_match_events: Vec<PortalSkillMatchEvent>,
    pub(super) tool_load_events: Vec<PortalToolLoadEvent>,
    pub(super) active_skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalLaunchTarget {
    pub(super) path: String,
    pub(super) group: String,
}

#[derive(Debug, Clone)]
pub(super) struct MaterializedLaunchTarget {
    pub(super) mode: String,
    pub(super) target_label: String,
    pub(super) launch_file: PathBuf,
    pub(super) workspace_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PortalLaunchJob {
    pub(super) id: String,
    pub(super) mode: String,
    pub(super) target_label: String,
    pub(super) status: String,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) logs: String,
    pub(super) discovered_run_paths: Vec<String>,
    pub(super) workspace_dir: Option<String>,
    pub(super) transcript_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalLaunchTargetList {
    pub(super) targets: Vec<PortalLaunchTarget>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalLaunchJobList {
    pub(super) jobs: Vec<PortalLaunchJob>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PortalLaunchRequest {
    pub(super) file_path: Option<String>,
    pub(super) source: Option<String>,
    pub(super) task: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PortalTriggerReplayRequest {
    pub(super) event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PortalRunDiff {
    pub(super) left_path: String,
    pub(super) right_path: String,
    pub(super) identical: bool,
    pub(super) status_changed: bool,
    pub(super) left_status: String,
    pub(super) right_status: String,
    pub(super) stage_diffs: Vec<harn_vm::orchestration::RunStageDiffRecord>,
    pub(super) tool_diffs: Vec<harn_vm::orchestration::ToolCallDiffRecord>,
    pub(super) observability_diffs: Vec<harn_vm::orchestration::RunObservabilityDiffRecord>,
    pub(super) transition_count_delta: isize,
    pub(super) artifact_count_delta: isize,
    pub(super) checkpoint_count_delta: isize,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalListResponse {
    pub(super) stats: PortalStats,
    pub(super) filtered_count: usize,
    pub(super) pagination: PortalPagination,
    pub(super) runs: Vec<PortalRunSummary>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalPagination {
    pub(super) page: usize,
    pub(super) page_size: usize,
    pub(super) total_pages: usize,
    pub(super) total_runs: usize,
    pub(super) has_previous: bool,
    pub(super) has_next: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalMeta {
    pub(super) workspace_root: String,
    pub(super) run_dir: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalHighlightKeywords {
    pub(super) keyword: Vec<String>,
    pub(super) literal: Vec<String>,
    pub(super) built_in: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalLlmProviderOption {
    pub(super) name: String,
    pub(super) base_url: String,
    pub(super) base_url_env: Option<String>,
    pub(super) auth_style: String,
    pub(super) auth_envs: Vec<String>,
    pub(super) auth_configured: bool,
    pub(super) viable: bool,
    pub(super) local: bool,
    pub(super) models: Vec<String>,
    pub(super) aliases: Vec<String>,
    pub(super) default_model: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PortalLlmOptions {
    pub(super) preferred_provider: Option<String>,
    pub(super) preferred_model: Option<String>,
    pub(super) providers: Vec<PortalLlmProviderOption>,
}
