//! Harn Flow — agent-native shipping substrate.
//!
//! See parent epic #571 for the four-primitive model (atoms, intents, slices,
//! streams). This module currently implements the foundational primitives,
//! [`Atom`](atom::Atom), [`Intent`](intent::Intent), and [`Slice`](slice::Slice).

pub mod atom;
pub mod backend;
pub mod intent;
pub mod predicates;
pub mod slice;
pub mod store;

pub use atom::{Atom, AtomError, AtomId, AtomSignature, Provenance, TextOp};
pub use backend::{
    AtomRef, FlowNativeBackend, FlowSlice, GitExportReceipt, ShadowGitBackend, ShipReceipt,
    VcsBackend, VcsBackendError,
};
pub use intent::{
    Intent, IntentBoundaryClassifier, IntentBoundaryDecision, IntentBoundaryDispute,
    IntentClusterOptions, IntentClusterer, IntentError, IntentId, ObservedAtom, SealedIntent,
    TranscriptSpan,
};
pub use predicates::{
    discover_invariants, parse_invariants_source, resolve_predicates, ArchivistMetadata,
    CheapJudge, CheapJudgeRequest, CheapJudgeResponse, DiscoveredInvariantFile,
    DiscoveredPredicate, DiscoveryDiagnostic, DiscoveryDiagnosticSeverity, ParsedInvariantFile,
    PredicateContext, PredicateExecutionRecord, PredicateExecutionReport, PredicateExecutor,
    PredicateExecutorConfig, PredicateKind, PredicateRunner, INVARIANTS_FILE,
};
pub use slice::{
    derive_slice, Approval, CoverageMap, InvariantBlockError, InvariantResult, PredicateHash,
    Slice, SliceDerivationError, SliceDerivationInput, SliceId, SliceStatus, TestId,
    UnresolvedParent,
};
pub use store::{AtomDelta, SqliteFlowStore, StateVector};
