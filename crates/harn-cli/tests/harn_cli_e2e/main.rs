//! Consolidated e2e integration-test binary for the `harn-cli` crate.
//!
//! Every e2e `harn-cli` integration test compiles into this single binary.
//! Each former `tests/<name>.rs` file is now a submodule at
//! `tests/harn_cli_e2e/<name>.rs`. Collapsing the ~92 separate test binaries into two
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

mod acp_registry_manifest;
mod acp_server_cli;
mod agent_run_command_argv_coercion;
mod artifact_manifest_schema;
mod attributed_decl_cli;
mod canon_dispatch;
mod check_fmt_json_cli;
mod check_result_cache;
mod check_strict_cli;
mod codemod_dispatch;
mod conformance_json_cli;
mod coverage_cli;
mod demo_cli_e2e;
mod dev_cli;
mod dispatch_aot;
mod dispatch_echo;
mod dispatch_snapshot;
mod doctor_cli;
mod doctor_dispatch;
mod eval_cluster_dispatch;
mod eval_coding_agent_cli;
mod eval_coding_agent_dispatch;
mod eval_context_cli;
mod eval_prompt_dispatch;
mod eval_skill_gate_cli;
mod explain_dispatch;
mod graph_cli;
mod harn_script_lint_rules_dispatch;
mod harn_serve_mcp_cli;
mod host_lease_cli;
#[cfg(unix)]
mod host_lease_crash_cli;
mod json_schemas_cli;
mod lint_changed_cli;
mod lint_fix_exit_cli;
mod lint_replay_version_upgrade_json_cli;
mod lint_strict_cli;
mod mcp_server_cli;
mod merge_captain_cli;
mod merge_captain_mock_cli;
mod models_dispatch;
mod native_rule_libraries;
mod orchestrator_cli;
#[cfg(unix)]
mod orchestrator_cli_e2e;
#[cfg(any())]
mod orchestrator_inbox_dedupe;
mod package_generation_concurrency_cli_e2e;
mod parse_tokens_cli;
mod persona_activation_cli_e2e;
mod pg_codegen_cli;
mod precompile_dispatch;
mod provider_dispatch_audit;
mod providers_dispatch;
mod replay_session_cli;
mod routes_cli;
mod routes_graph_dispatch;
mod rule_discovery_dispatch;
mod rule_pack_install_dispatch;
mod rule_test_dispatch;
mod run_eval_cleanup_e2e;
mod run_eval_imports;
mod run_exit_codes;
mod run_json_cli;
mod runs_export_training_cli;
mod runs_view_cli;
mod scaffold_dispatch;
mod scan_dispatch;
#[cfg(unix)]
mod sidecar_version_cli;
mod skills_cli;
mod supervisor_cli;
mod test_worker_cli_e2e;
mod time_cli;
mod trace_import_dispatch;
mod try_dispatch;
mod usage_cli;
mod user_test_cli;
mod user_test_reports_cli;
mod version_dispatch;
mod workflow_authoring_eval;
mod workflow_cli;
mod workflow_patch_cli;
