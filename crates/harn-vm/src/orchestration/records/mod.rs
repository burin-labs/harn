//! Run record types, eval-pack evaluation, action-graph builders, persistence, and diff helpers.
//!
//! Focused submodules keep the run-record surface cohesive while
//! `orchestration::mod.rs` re-exports `records::*` as the public API.

mod action_graph;
mod diff;
mod eval_pack;
mod json;
mod persistence;
mod types;

pub use action_graph::{append_action_graph_update, derive_run_observability};
pub use diff::{diff_run_records, render_unified_diff};
pub use eval_pack::{
    eval_pack_case_fingerprint, eval_pack_harness_config_fingerprint, evaluate_eval_pack_manifest,
    evaluate_run_against_fixture, evaluate_run_suite, evaluate_run_suite_manifest,
    load_eval_pack_manifest, load_eval_suite_manifest, normalize_eval_pack_manifest_value,
    normalize_eval_suite_manifest, replay_fixture_from_run, validate_eval_pack_split,
};
pub use persistence::{
    load_agent_session_replay_events, load_agent_session_replay_events_from_log, load_run_record,
    normalize_run_record, save_run_record, AgentSessionReplayEvent,
};
pub use types::{
    tool_fixture_hash, ClarifyingQuestionEvalSpec, CompactionEventRecord, DaemonEventKindRecord,
    DaemonEventRecord, EvalPackAssertion, EvalPackCase, EvalPackCaseReport, EvalPackDefaults,
    EvalPackFixtureRef, EvalPackGoldenExample, EvalPackJudgeConfig, EvalPackManifest,
    EvalPackPackage, EvalPackReliabilityBreakdown, EvalPackReliabilityReport, EvalPackReport,
    EvalPackRubric, EvalPackSplit, EvalPackSplitValidationReport, EvalPackStatsReport,
    EvalPackStatsRow, EvalPackThresholds, EvalPackTrialReport, EvalSuiteCase, EvalSuiteManifest,
    LlmUsageRecord, ReplayEvalCaseReport, ReplayEvalReport, ReplayEvalSuiteReport, ReplayFixture,
    ReplayStageAssertion, RunActionGraphEdgeRecord, RunActionGraphNodeRecord, RunCheckpointRecord,
    RunChildRecord, RunDeliverableSummaryRecord, RunDiffReport, RunExecutionRecord,
    RunHitlQuestionRecord, RunObservabilityDiffRecord, RunObservabilityRecord,
    RunPersonaRuntimeRecord, RunPlannerRoundRecord, RunRecord, RunStageAttemptRecord,
    RunStageDiffRecord, RunStageRecord, RunTaskLedgerSummaryRecord, RunTraceSpanRecord,
    RunTranscriptPointerRecord, RunTransitionRecord, RunVerificationOutcomeRecord,
    RunWorkerLineageRecord, ToolCallDiffRecord, ToolCallRecord,
    ACTION_GRAPH_EDGE_KIND_A2A_DISPATCH, ACTION_GRAPH_EDGE_KIND_DELEGATES,
    ACTION_GRAPH_EDGE_KIND_DLQ_MOVE, ACTION_GRAPH_EDGE_KIND_ENTRY,
    ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE, ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN,
    ACTION_GRAPH_EDGE_KIND_RETRY, ACTION_GRAPH_EDGE_KIND_TRANSITION,
    ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH, ACTION_GRAPH_NODE_KIND_A2A_HOP,
    ACTION_GRAPH_NODE_KIND_DISPATCH, ACTION_GRAPH_NODE_KIND_DLQ, ACTION_GRAPH_NODE_KIND_PREDICATE,
    ACTION_GRAPH_NODE_KIND_RETRY, ACTION_GRAPH_NODE_KIND_RUN, ACTION_GRAPH_NODE_KIND_STAGE,
    ACTION_GRAPH_NODE_KIND_TRIGGER, ACTION_GRAPH_NODE_KIND_TRIGGER_PREDICATE,
    ACTION_GRAPH_NODE_KIND_WORKER, ACTION_GRAPH_NODE_KIND_WORKER_ENQUEUE,
};

pub(crate) use types::run_child_record_from_worker_metadata;

#[cfg(test)]
pub(crate) use diff::{myers_diff, DiffOp};

#[cfg(test)]
#[path = "../records_tests.rs"]
mod records_tests;
