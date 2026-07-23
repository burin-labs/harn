//! Consolidated fast integration-test binary for the `harn-cli` crate.
//!
//! Every fast `harn-cli` integration test compiles into this single binary.
//! Each former `tests/<name>.rs` file is now a submodule at
//! `tests/harn_cli_fast/<name>.rs`. Collapsing the ~92 separate test binaries into two
//! (harn_cli_fast + harn_cli_e2e) cuts link time and shrinks the nextest archive; the two
//! binary names are load-bearing (the nextest default/ci profiles filter the
//! fast suite with `package(harn-cli) and binary(harn_cli_fast)`; the e2e
//! profile runs `package(harn-cli) and kind(test)`, i.e. both binaries).
//!
//! `recursion_limit` is a crate-level attribute set once here; the former
//! per-file `#![recursion_limit = "256"]` declarations have no effect inside a
//! module and are consolidated to this root.
#![recursion_limit = "256"]

// Shared helpers (still at `tests/test_util/`, one level up), reached by the
// submodules via `crate::test_util::...`.
#[path = "../test_util/mod.rs"]
mod test_util;

mod burin_mini_playground;
mod bytecode_cache;
mod check_cli;
mod crystallize_cli;
mod demo_cli;
mod eval_prompt_cli;
mod flow_ship_cli;
mod harnpack_run;
mod llm_mock_cli;
#[cfg(unix)]
mod orchestrator_http;
mod pack_cli;
mod persona_cli;
mod profile;
mod test_bench_cli;
mod trigger_replay_cli;
