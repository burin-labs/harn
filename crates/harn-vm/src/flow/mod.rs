//! Harn Flow — agent-native shipping substrate.
//!
//! See parent epic #571 for the four-primitive model (atoms, intents, slices,
//! streams). This module currently implements the foundational primitives,
//! [`Atom`](atom::Atom), [`Intent`](intent::Intent), and [`Slice`](slice::Slice).

pub mod atom;
pub mod audit;
pub mod backend;
pub mod fixer;
pub mod intent;
pub mod predicates;
pub mod slice;
pub mod store;

pub use atom::{Atom, AtomError, AtomId, AtomSignature, Provenance, TextOp};
pub use audit::{
    audit_slice_against_current_predicates, replay_audit_report, ReplayAuditPredicate,
    ReplayAuditReport, SliceReplayAudit,
};
pub use backend::{
    AtomRef, FlowNativeBackend, FlowSlice, GitExportReceipt, ShadowGitBackend, ShipReceipt,
    VcsBackend, VcsBackendError,
};
pub use fixer::{
    propose_follow_up_slice, FixerError, FixerFollowUpProposal, FixerProposalInput, FixerReceipt,
    FixerSigningContext, FIXER_PERSONA_NAME, FIXER_TRIGGER,
};
pub use intent::{
    Intent, IntentBoundaryClassifier, IntentBoundaryDecision, IntentBoundaryDispute,
    IntentClusterOptions, IntentClusterer, IntentError, IntentId, ObservedAtom, SealedIntent,
    TranscriptSpan,
};
pub use predicates::{
    compose_predicate_results, discover_invariants, parse_invariants_source, resolve_predicates,
    resolve_predicates_for_touched_directories, Approver, ArchivistMetadata, ByteSpan, CheapJudge,
    CheapJudgeRequest, CheapJudgeResponse, ComposedPredicateEvaluation, DiscoveredInvariantFile,
    DiscoveredPredicate, DiscoveryDiagnostic, DiscoveryDiagnosticSeverity, EvidenceItem,
    InvariantBlockError, InvariantResult, ParsedInvariantFile, PredicateContext,
    PredicateEvaluation, PredicateExecutionRecord, PredicateExecutionReport, PredicateExecutor,
    PredicateExecutorConfig, PredicateKind, PredicateRunner, PredicateSource, Remediation,
    ResolvedPredicate, Verdict, VerdictStrictness, INVARIANTS_FILE,
};
pub use slice::{
    derive_slice, Approval, CoverageMap, PredicateHash, Slice, SliceDerivationError,
    SliceDerivationInput, SliceId, SliceStatus, TestId, UnresolvedParent,
};
pub use store::{AtomDelta, SqliteFlowStore, StateVector, StoredDerivedSlice};
