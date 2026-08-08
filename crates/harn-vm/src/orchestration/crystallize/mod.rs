//! Workflow-candidate crystallization, mining, code generation, shadow execution, and bundling.
//!
//! Split out of the original `crystallize.rs` (~3.3k lines) into focused submodules. The parent
//! `orchestration::mod.rs` re-exports `crystallize::*`, so every public item below stays
//! reachable via its previous path.

mod api;
mod bundle;
mod codegen;
mod normalize;
mod shadow;
mod skill;
mod trajectory;
mod types;
mod util;

pub use api::{
    crystallize_traces, load_crystallization_trace, load_crystallization_traces_from_dir,
    synthesize_candidate_from_trace, write_crystallization_artifacts,
};
pub use bundle::{
    build_crystallization_bundle, load_crystallization_bundle,
    load_crystallization_bundle_manifest, shadow_replay_bundle, validate_crystallization_bundle,
    write_crystallization_bundle, BundleEvalPackRef, BundleFixtureRef, BundleGenerator, BundleKind,
    BundleOptions, BundlePromotion, BundleRedactionSummary, BundleSkillRef, BundleSourceTrace,
    BundleStep, BundleValidation, BundleWorkflowRef, CrystallizationBundle,
    CrystallizationBundleManifest,
};
pub use codegen::{generate_eval_pack, generate_harn_code};
pub use skill::{induce_skill_candidate, refresh_skill_candidates};
pub use trajectory::{
    apply_trajectory_verifier, ingest_agent_loop_trajectory, turn_record, AgentTurnRecord,
    AgentTurnToolCall, TrajectoryIngestResult, TrajectoryTap, TRAJECTORY_SOURCE,
};
pub use types::{
    CrystallizationAction, CrystallizationApproval, CrystallizationArtifacts, CrystallizationCost,
    CrystallizationFlowRef, CrystallizationInputFormat, CrystallizationReport,
    CrystallizationSideEffect, CrystallizationTrace, CrystallizationUsage, CrystallizeOptions,
    PromotionApprovalRecord, PromotionCriteria, PromotionDivergenceRecord, PromotionMetadata,
    PromotionStatus, RecoveryFeedbackSummary, SavingsEstimate, SegmentKind, SegmentSummary,
    ShadowRunReport, ShadowTraceResult, SkillCandidateArtifact, SkillCandidateEvidenceRef,
    SkillCandidateEvidenceRole, SkillInductionGateReceipt, SkillInductionReplayGate,
    WorkflowCandidate, WorkflowCandidateExample, WorkflowCandidateParameter, WorkflowCandidateStep,
    WorkflowClusterKey, BUNDLE_EVAL_PACK_FILE, BUNDLE_FIXTURES_DIR, BUNDLE_MANIFEST_FILE,
    BUNDLE_REPORT_FILE, BUNDLE_SCHEMA, BUNDLE_SCHEMA_VERSION, BUNDLE_SKILL_DIR, BUNDLE_SKILL_FILE,
    BUNDLE_SKILL_GATE_FILE, BUNDLE_WORKFLOW_FILE, SKILL_CANDIDATE_SCHEMA,
    SKILL_CANDIDATE_SCHEMA_VERSION, SKILL_GATE_RECEIPT_SCHEMA,
};

#[cfg(test)]
#[path = "../crystallize_tests.rs"]
mod crystallize_tests;
