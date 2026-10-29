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

#[path = "../support/required_pr_e2e.rs"]
mod required_pr_e2e;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod acp_confinement_cli;
mod acp_registry_manifest;
mod acp_server_cli;
mod agent_run_command_argv_coercion;
mod artifact_manifest_schema;
mod attributed_decl_cli;
mod bubblewrap_fallback_e2e;
mod cache_dir_cli;
mod canon_dispatch;
mod check_fmt_json_cli;
mod check_result_cache;
mod check_strict_cli;
mod checkpoint_concurrency;
mod codemod_dispatch;
mod command_probe_parent_liveness;
mod conformance_json_cli;
mod conformance_process_lifetime_e2e;
#[cfg(unix)]
mod connect_legacy_recovery;
#[cfg(unix)]
mod connect_secret_env_redaction;
#[cfg(unix)]
mod connect_secret_store;
mod conservative_experiment;
mod coverage_cli;
mod decision_route_admission;
mod demo_cli_e2e;
mod dev_cli;
mod dispatch_aot;
mod dispatch_echo;
mod dispatch_snapshot;
mod doctor_cli;
mod doctor_dispatch;
mod durable_spend_policy;
mod environment_registry_cli;
mod eval_calibrate_cli;
mod eval_cluster_dispatch;
mod eval_coding_agent_cli;
mod eval_coding_agent_dispatch;
mod eval_context_cli;
mod eval_prompt_dispatch;
mod eval_skill_gate_cli;
mod evaluation_receipt_verification;
mod explain_dispatch;
mod git_config_sandbox_cli;
mod graph_cli;
mod guardian_namespace_e2e;
mod harn_script_lint_rules_dispatch;
mod harn_serve_api_cli;
mod harn_serve_mcp_cli;
mod host_lease_cli;
#[cfg(unix)]
mod host_lease_crash_cli;
mod hosted_worker_connectors_e2e;
mod in_process_grant;
mod json_schemas_cli;
mod lint_changed_cli;
mod lint_fix_exit_cli;
mod lint_replay_version_upgrade_json_cli;
mod lint_strict_cli;
mod llm_fixture_policy;
mod mcp_lazy_registration;
mod mcp_server_cli;
mod merge_captain_cli;
mod merge_captain_mock_cli;
mod model_context;
mod models_dispatch;
mod native_rule_libraries;
mod operator_syntax_migration_cli;
mod orchestrator_cli;
#[cfg(unix)]
mod orchestrator_cli_e2e;
#[cfg(any())]
mod orchestrator_inbox_dedupe;
mod package_cache_sandbox_cli;
mod package_generation_concurrency_cli_e2e;
mod package_manager_roots_cli;
mod package_registry_verify_cli;
mod package_verify_cli;
mod parse_tokens_cli;
mod path_metadata_persistence_cli;
mod persona_activation_cli_e2e;
mod pg_codegen_cli;
mod portal_stderr;
mod precompile_dispatch;
mod precompiled_interfaces;
mod predicate_contract;
#[cfg(target_os = "macos")]
mod process_smoke_cli;
mod provider_account_confirmation;
mod provider_admission_privacy;
mod provider_catalog_sources;
mod provider_dispatch_audit;
mod provider_option_qualification;
mod providers_dispatch;
#[cfg(unix)]
mod relative_path_resolution_cli;
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
mod scoped_ssh_agent;
mod secret_absence;
mod shard_coverage_cli;
#[cfg(unix)]
mod sidecar_version_cli;
mod signals_hook_cli;
mod skills_cli;
mod supervisor_cli;
mod test_worker_cli_e2e;
mod time_cli;
mod tool_registry_cli;
mod trace_import_dispatch;
mod trace_prefix_stability_cli;
mod trusted_host_dispatch_cli;
mod try_dispatch;
mod typed_tool_outcomes;
mod usage_cli;
mod user_test_cli;
mod user_test_reports_cli;
mod version_dispatch;
mod workflow_authoring_eval;
mod workflow_cli;
mod workflow_patch_cli;

macro_rules! check_required_cases {
    ($($(#[$attribute:meta])* $case:path,)*) => {
        $(
            $(#[$attribute])*
            const _: fn() = $case;
        )*
    };
}

required_pr_e2e::required_pr_e2e_cases!(check_required_cases);
const _: usize = required_pr_e2e::CASES.len();
