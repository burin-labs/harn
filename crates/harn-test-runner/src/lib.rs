//! Reusable user-test execution contracts.
//!
//! This crate is the compile boundary for Harn's test engine. Command-line
//! parsing and rendering remain in `harn-cli`; test discovery, scheduling,
//! execution state, and machine-readable receipts live here so focused runner
//! work does not compile the entire CLI product graph.

mod callable;
mod discovery;
mod fixtures;
mod model;
mod reporting;
mod scheduling;
mod session;
mod timing;

#[doc(hidden)]
pub use callable::{prepare_callable_entries, PreparedCallableCases};
#[doc(hidden)]
pub use discovery::{extract_cases_from_program, parse_program, seed_imported_enum_candidates};
#[doc(hidden)]
pub use fixtures::{FixtureScope, TestFixture};
#[doc(hidden)]
pub use model::TestCase;
pub use reporting::{
    AggregateTimings, CostRegression, DominantCase, PhaseTimings, ShardPlan,
    SuiteCallablePreparation, SuiteModulePreparation, TestPhase, TestResult, TestSummary,
    TestTimeout, TestTimingSpan,
};
#[doc(hidden)]
pub use scheduling::{execute_parallel_cases, ParallelCaseResults, ParallelRunOptions};
pub use scheduling::{TestRunEvent, TestRunProgress};
pub use session::{TestRunSession, TestRunSessionStats};
pub use timing::DurationSummary;
