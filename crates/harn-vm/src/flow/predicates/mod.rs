//! Flow invariant predicate execution.

pub mod discovery;
pub mod executor;

pub use discovery::{
    discover_invariants, parse_invariants_source, resolve_predicates, ArchivistMetadata,
    DiagnosticSeverity as DiscoveryDiagnosticSeverity, DiscoveredInvariantFile,
    DiscoveredPredicate, DiscoveryDiagnostic, ParsedInvariantFile, INVARIANTS_FILE,
};
pub use executor::{
    CheapJudge, CheapJudgeRequest, CheapJudgeResponse, PredicateContext, PredicateExecutionRecord,
    PredicateExecutionReport, PredicateExecutor, PredicateExecutorConfig, PredicateKind,
    PredicateRunner,
};
