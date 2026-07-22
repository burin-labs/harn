//! Consolidated integration-test binary for the `harn-vm` crate.
//!
//! Most `harn-vm` integration tests compile into this single binary. Each
//! consolidated `tests/<name>.rs` file is now a submodule at
//! `tests/harn_vm/<name>.rs`, declared below. Collapsing 36 separate test
//! binaries into one cuts link time and shrinks the nextest archive. Three
//! tests remain separate because they rely on process isolation for global
//! allocator or inbox state. The binary name `harn_vm` is load-bearing (CI filters use
//! `package(harn-vm) and binary(harn_vm)`).
//!
//! `recursion_limit` is set here at the crate root because it is a
//! crate-level attribute: the former per-file `#![recursion_limit = "256"]`
//! declarations have no effect inside a module and are consolidated here.
#![recursion_limit = "256"]

// Shared helpers (still at `tests/support/mod.rs`, one level up), reached by
// the process-options and session-profile submodules via `crate::support`.
#[path = "../support/mod.rs"]
mod support;

mod agent_fanout;
mod agent_loop_final_wrapup;
mod agent_loop_output_schema;
mod agent_loop_steering_seams;
mod agent_mcp_mid_conversation;
mod agent_mcp_tool_ceiling;
mod agent_sessions;
mod builtin_call_dispatch;
mod builtin_registry_alignment;
mod builtin_signature_text_drift;
mod cache_conformance_fixtures;
mod codegen_fingerprint;
mod compaction_policy_primitive;
mod connector_testkit_public_api;
mod flow_backend;
mod github_stdlib_connectors;
mod injection_classifier_loader;
mod mcp_call_budget;
mod orchestration_cutover;
// Formerly gated by a file-level `#![cfg(feature = "otel")]`; preserved as a
// module-level gate so these tests exist only under `--features otel`.
#[cfg(feature = "otel")]
mod otel_sink_export;
mod persona_policy_public_api;
mod pool_multithread;
mod process_options_cross_platform;
mod redaction_fixtures;
mod run_view_fixtures;
mod runtime_introspection;
mod sandbox_hardened;
mod session_profile_env_leak;
mod skill_activation_evidence_conformance;
mod thread_local_audit;
mod tool_call_cancellation;
mod tool_calling_bootcamp;
mod tool_ref;
mod trajectory_tap;
mod worker_overlap;
mod workflow_replay_byte_compat;
