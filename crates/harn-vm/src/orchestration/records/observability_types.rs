//! Observability projections of a run record: planner rounds, action graph,
//! worker lineage, verification, transcript pointers, and daemon events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunDeliverableSummary")]
pub struct RunDeliverableSummaryRecord {
    pub id: String,
    pub text: String,
    pub status: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunTaskLedgerSummary")]
pub struct RunTaskLedgerSummaryRecord {
    pub root_task: String,
    pub rationale: String,
    pub deliverables: Vec<RunDeliverableSummaryRecord>,
    pub observations: Vec<String>,
    pub blocking_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunPlannerRound")]
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
#[derive(ts_rs::TS)]
#[ts(rename = "RunWorkerLineage")]
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
#[derive(ts_rs::TS)]
#[ts(rename = "RunActionGraphNode")]
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
#[derive(ts_rs::TS)]
#[ts(rename = "RunActionGraphEdge")]
pub struct RunActionGraphEdgeRecord {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunVerificationOutcome")]
pub struct RunVerificationOutcomeRecord {
    pub stage_id: String,
    pub node_id: String,
    pub status: String,
    pub passed: Option<bool>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunTranscriptPointer")]
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
#[derive(ts_rs::TS)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CompactionEventRecord {
    /// Schema version of the receipt this record was projected from. `0` marks a
    /// record migrated from a legacy transcript that predates the embedded
    /// receipt (harn#4995), reconstructed from the flat event metadata.
    pub schema_version: u32,
    /// Stable identity, equal to the transcript event id and the receipt id.
    pub id: String,
    pub transcript_id: Option<String>,
    pub stage_id: Option<String>,
    pub node_id: Option<String>,
    pub mode: String,
    /// Why compaction fired. Survives projection from the receipt; empty only
    /// for legacy transcripts that never recorded a reason.
    pub reason: String,
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
    /// Observation-mask recap metrics; `None` for non-masking strategies.
    pub recap: Option<super::super::RecapMetrics>,
    pub classification: Option<Box<super::super::ClassificationReceipt>>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
#[derive(ts_rs::TS)]
#[ts(rename = "DaemonEventKind")]
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
#[derive(ts_rs::TS)]
#[ts(rename = "DaemonEvent")]
pub struct DaemonEventRecord {
    pub daemon_id: String,
    pub name: String,
    pub kind: DaemonEventKindRecord,
    pub timestamp: String,
    pub persist_path: String,
    pub payload_summary: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(ts_rs::TS)]
#[ts(rename = "RunObservability")]
pub struct RunObservabilityRecord {
    pub schema_version: usize,
    pub planner_rounds: Vec<RunPlannerRoundRecord>,
    pub research_fact_count: usize,
    pub action_graph_nodes: Vec<RunActionGraphNodeRecord>,
    pub action_graph_edges: Vec<RunActionGraphEdgeRecord>,
    pub worker_lineage: Vec<RunWorkerLineageRecord>,
    pub verification_outcomes: Vec<RunVerificationOutcomeRecord>,
    pub transcript_pointers: Vec<RunTranscriptPointerRecord>,
    #[ts(skip)]
    pub compaction_events: Vec<CompactionEventRecord>,
    pub daemon_events: Vec<DaemonEventRecord>,
}
