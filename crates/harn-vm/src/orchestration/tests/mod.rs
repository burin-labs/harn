//! Orchestration integration tests, grouped by the behavior under test.
//!
//! Wiring only — each module below is tests (plus, where a test needs one,
//! its own fixture helpers).

mod arg_constraints;
mod artifacts;
mod compaction;
mod hooks;
mod payload_errors;
mod policy;
mod registered_closures;
mod replay_eval;
mod run_observability;
mod run_records;
mod settlement;
mod side_effect_ceiling;
mod unified_diff;
mod workflow;
