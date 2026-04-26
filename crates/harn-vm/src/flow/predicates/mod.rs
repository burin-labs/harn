//! Flow invariant predicate execution.

pub mod compose;
pub mod discovery;
pub mod executor;
pub mod result;

pub use compose::{
    compose_predicate_results, resolve_predicates, resolve_predicates_for_touched_directories,
    ComposedPredicateEvaluation, PredicateEvaluation, PredicateSource, ResolvedPredicate,
    VerdictStrictness,
};
pub use discovery::{
    discover_invariants, parse_invariants_source, ArchivistMetadata,
    DiagnosticSeverity as DiscoveryDiagnosticSeverity, DiscoveredInvariantFile,
    DiscoveredPredicate, DiscoveryDiagnostic, ParsedInvariantFile, INVARIANTS_FILE,
};
pub use executor::{
    CheapJudge, CheapJudgeRequest, CheapJudgeResponse, PredicateContext, PredicateExecutionRecord,
    PredicateExecutionReport, PredicateExecutor, PredicateExecutorConfig, PredicateKind,
    PredicateRunner, SemanticReplayAuditMetadata,
};
pub use result::{
    Approver, ByteSpan, EvidenceItem, InvariantBlockError, InvariantResult, Remediation, Verdict,
};
