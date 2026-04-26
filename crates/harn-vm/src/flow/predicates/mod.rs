//! Flow invariant predicate execution.

pub mod executor;

pub use executor::{
    CheapJudge, CheapJudgeRequest, CheapJudgeResponse, PredicateContext, PredicateExecutionRecord,
    PredicateExecutionReport, PredicateExecutor, PredicateExecutorConfig, PredicateKind,
    PredicateRunner,
};
