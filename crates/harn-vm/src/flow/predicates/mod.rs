//! Flow invariant predicate execution.

pub mod bootstrap;
pub mod compose;
pub mod discovery;
pub mod executor;
pub mod result;

pub use bootstrap::{
    discover_bootstrap_policy, validate_bootstrap_edit, validate_predicate_edit, BootstrapPolicy,
    BootstrapValidation, BootstrapViolation, DiscoveredBootstrapPolicy, EditAuthor,
    DEFAULT_MAINTAINER_ROLE, META_INVARIANTS_FILE,
};
pub use compose::{
    compose_predicate_results, enforce_predicate_ceiling, resolve_predicates,
    resolve_predicates_for_touched_directories, ComposedPredicateEvaluation, DirectoryContribution,
    PredicateCeiling, PredicateCeilingLevel, PredicateCeilingOutcome, PredicateCeilingViolation,
    PredicateEvaluation, PredicateSource, ResolvedPredicate, VerdictStrictness,
    PREDICATE_COUNT_EXPLOSION_CODE,
};
pub use discovery::{
    discover_invariants, parse_invariants_source, ArchivistMetadata,
    DiagnosticSeverity as DiscoveryDiagnosticSeverity, DiscoveredInvariantFile,
    DiscoveredPredicate, DiscoveryDiagnostic, ParsedInvariantFile, INVARIANTS_FILE,
};
pub use executor::{
    CheapJudge, CheapJudgeRequest, CheapJudgeResponse, PredicateContext, PredicateExecutionRecord,
    PredicateExecutionReport, PredicateExecutor, PredicateExecutorConfig, PredicateKind,
    PredicateRunner, PredicateSchedulerConfig, SemanticReplayAuditMetadata,
};
pub use result::{
    Approver, ByteSpan, EvidenceItem, InvariantBlockError, InvariantResult, Remediation, Verdict,
};
