//! Run record types, eval-pack evaluation, action-graph builders, persistence, and diff helpers.
//!
//! Focused submodules keep the run-record surface cohesive while
//! `orchestration::mod.rs` re-exports `records::*` as the public API.

mod action_graph;
mod diff;
mod eval_pack;
mod execution_evidence;
mod from_session;
mod json;
mod persistence;
mod report;
mod time;
mod transcript_descriptor;
mod types;
mod view;

pub use action_graph::{append_action_graph_update, derive_run_observability};
pub use diff::{diff_run_records, render_unified_diff};
pub use eval_pack::{
    eval_ledger_append_rows_report, eval_ledger_prior_commit_rows_report, eval_ledger_read_report,
    eval_ledger_resume_plan_report, eval_pack_case_fingerprint,
    eval_pack_harness_config_fingerprint, evaluate_eval_pack_manifest,
    evaluate_eval_pack_manifest_resumable,
    evaluate_eval_pack_manifest_resumable_with_live_executor,
    evaluate_eval_pack_manifest_with_live_executor, evaluate_run_against_fixture,
    evaluate_run_suite, evaluate_run_suite_manifest, load_eval_pack_manifest,
    load_eval_suite_manifest, normalize_eval_pack_manifest_value, normalize_eval_suite_manifest,
    replay_fixture_from_run, validate_eval_pack_split, EvalPackLiveExecutor,
    EvalPackLiveExecutorRequest, EvalPackLiveVerifyOutcome,
};
pub use execution_evidence::{
    validate_execution_evidence, validate_execution_id, ExecutionEvidenceValidationError,
    EXECUTION_EVIDENCE_SCHEMA_VERSION,
};
pub use from_session::{
    default_projection_path, list_session_runs, materialize_session_run_record,
    project_run_record_from_session, SessionRunSummary, AGENT_SESSION_WORKFLOW_ID,
    PROJECTION_SOURCE, UNRECOVERABLE_FIELDS,
};
#[cfg(test)]
pub(crate) use persistence::save_run_record_with_transcript;
pub use persistence::{
    load_agent_session_replay_events, load_agent_session_replay_events_from_log, load_run_record,
    normalize_run_record, prune_execution_run_records, save_execution_run_record, save_run_record,
    AgentSessionReplayEvent, DEFAULT_EXECUTION_RUN_RETENTION,
};
pub(crate) use report::read_checked_run_report_bytes;
pub use report::{
    build_run_report, run_report_projection_hash, validate_run_report, RunReport, RunReportAgent,
    RunReportCheck, RunReportCoordination, RunReportDelegation, RunReportError, RunReportExecution,
    RunReportLlmCall, RunReportProjection, RunReportRequest, RunReportSource, RunReportToolCall,
    RunReportValidationError, RUN_REPORT_SCHEMA, RUN_REPORT_SCHEMA_VERSION,
};
pub use transcript_descriptor::{
    describe_llm_transcript_sidecar, verified_llm_transcript_pointer_path,
    LlmTranscriptDescriptorError,
};
pub use types::{
    tool_fixture_hash, ClarifyingQuestionEvalSpec, CompactionEventRecord, DaemonEventKindRecord,
    DaemonEventRecord, EvalLedgerAppendReport, EvalLedgerFingerprintMismatch,
    EvalLedgerPriorCommitReport, EvalLedgerProvenance, EvalLedgerReadReport, EvalLedgerResumeCell,
    EvalLedgerResumePlan, EvalLedgerRow, EvalPackAssertion, EvalPackCase, EvalPackCaseReport,
    EvalPackCommandObject, EvalPackCommandSpec, EvalPackDefaults, EvalPackFixtureRef,
    EvalPackGoldenExample, EvalPackJudgeConfig, EvalPackManifest, EvalPackPackage,
    EvalPackReliabilityBreakdown, EvalPackReliabilityReport, EvalPackReport, EvalPackRubric,
    EvalPackRunState, EvalPackSplit, EvalPackSplitValidationReport, EvalPackStatsReport,
    EvalPackStatsRow, EvalPackThresholds, EvalPackTrialReport, EvalSuiteCase, EvalSuiteManifest,
    ExecutionEvidenceRecord, LlmUsageRecord, ReplayEvalCaseReport, ReplayEvalReport,
    ReplayEvalSuiteReport, ReplayFixture, ReplayStageAssertion, RunActionGraphEdgeRecord,
    RunActionGraphNodeRecord, RunCheckpointRecord, RunChildRecord, RunDeliverableSummaryRecord,
    RunDiffReport, RunEvidenceGapRecord, RunExecutionRecord, RunHitlQuestionRecord,
    RunObservabilityDiffRecord, RunObservabilityRecord, RunPersonaRuntimeRecord,
    RunPlannerRoundRecord, RunRecord, RunStageAttemptRecord, RunStageDiffRecord, RunStageRecord,
    RunTaskLedgerSummaryRecord, RunTraceSpanRecord, RunTranscriptArtifactDescriptor,
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
pub use view::{
    build_empty_session_view, build_run_view, build_run_view_with_event_log,
    build_run_view_with_options, build_run_view_with_path, build_session_view_from_run_records,
    build_session_view_from_run_views, ProjectionInfo, RunView, RunViewApproval, RunViewArtifact,
    RunViewAuth, RunViewCheckpoint, RunViewChild, RunViewError, RunViewFailure, RunViewMetadata,
    RunViewOptions, RunViewPendingState, RunViewProvider, RunViewRun, RunViewStage, RunViewUsage,
    SessionView, SessionViewHistoryItem, SessionViewMetadata, SessionViewOptions,
    SessionViewSession, TranscriptSummary, ViewProducer, RUN_VIEW_SCHEMA, RUN_VIEW_SCHEMA_VERSION,
    SESSION_VIEW_QUERY_METHOD, SESSION_VIEW_SCHEMA, SESSION_VIEW_SCHEMA_VERSION,
};

pub(crate) use types::run_child_record_from_worker_metadata;

#[cfg(test)]
#[path = "../records_tests.rs"]
mod records_tests;
