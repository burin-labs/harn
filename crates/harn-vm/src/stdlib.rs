//! Standard library builtins for the Harn VM.
//!
//! Every builtin is declared with the `#[harn_builtin]` proc-macro
//! (`crate::stdlib::macros::harn_builtin`). Each annotation emits a sibling
//! `static <FN>_DEF: VmBuiltinDef` carrying the signature, aliases, handler,
//! and metadata, and registers it into the workspace-global
//! [`macros::ALL_BUILTIN_DEFS`] distributed slice at link time. The CLI / LSP /
//! lint / serve / dap binaries call [`force_link`] to defeat rlib dead-code
//! stripping (linkme issue #36) so every static lands in the slice. Modules
//! still expose a `register_<module>_builtins(vm)` helper for ordered eager
//! registration (e.g. so `clock::timestamp` can override `process::timestamp`).
//! `register_vm_stdlib` calls those helpers in order and then installs the
//! aggregated signatures into the parser registry.
//!
//! See `CONTRIBUTING.md` ("Adding a stdlib builtin") for the full template.

pub mod macros;

mod agent_sessions;
pub mod agent_state;
pub(crate) mod agents;
mod agents_daemon;
mod artifact_emit;
pub(crate) mod assemble;
pub mod asset_paths;
mod bytes;
mod calendar;
mod channel_guardrails;
mod channels;
pub(crate) mod clock;
pub(crate) mod collections;
mod command_policy;
pub(crate) mod compaction;
mod compression;
mod concurrency;
mod connectors;
mod cookies;
mod cron;
mod crypto;
mod csv;
mod datetime;
mod document;
mod durable_step;
mod event_log;
mod external_agent;
pub(crate) mod files;
mod flow;
pub(crate) mod fs;
mod git;
pub(crate) mod git_topology;
mod grounding;
pub(crate) mod harn_entry;
pub(crate) mod hitl;
mod hitl_read;
pub mod host;
pub mod http_response;
pub(crate) mod io;
mod iter;
pub(crate) mod json;
mod json_query;
pub(crate) mod json_stream;
mod jsonrpc;
mod junit;
mod lifecycle_receipts;
mod logging;
pub mod long_running;
mod math;
pub(crate) mod memory;
mod monitors;
mod multipart;
mod net;
mod net_policy;
mod oauth_dynreg;
mod oauth_storage;
pub(crate) mod observability;
pub(crate) mod options;
mod package_snapshot;
pub(crate) use package_snapshot::PackageSnapshotRegistry;
mod path;
pub(crate) mod path_scope_guard;
pub(crate) mod pool;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::install_shared_pool_registry;
pub mod process;
pub(crate) mod process_spawn;
mod project;
mod project_catalog;
mod project_enrich;
mod regex;
mod review;
mod runtime_scope;
pub(crate) mod sandbox;
pub mod secret_scan;
pub(crate) mod session_store;
mod sets;
pub(crate) mod shapes;
mod skills;
#[cfg(feature = "sqlite")]
mod sqlite;
pub(crate) mod strings;
pub(crate) mod supervisor;
pub mod template;
mod testbench;
mod testing;
mod timing;
pub mod token_redaction;
pub(crate) mod tool_hooks;
pub(crate) mod tools;
pub mod tracing;
mod transcript_compact;
pub(crate) mod transcript_project;
mod triggers_stdlib;
mod tui;
mod types;
mod url_parse;
mod vision;
pub(crate) mod waitpoint;
mod waitpoints;
mod web;
pub mod workflow_messages;
pub(crate) mod xml;

use crate::http::register_http_builtins;
use crate::llm::register_llm_builtins;
use crate::mcp::register_mcp_builtins;
use crate::mcp_server::register_mcp_server_builtins;
use crate::vm::Vm;

pub(crate) use crate::schema::{json_to_vm_value, schema_result_value};
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    process::set_thread_source_dir(dir);
}

/// Register core builtins: pure/deterministic, no I/O.
pub fn register_core_stdlib(vm: &mut Vm) {
    crate::runtime_context::register_runtime_context_builtins(vm);
    types::register_type_builtins(vm);
    math::register_math_builtins(vm);
    strings::register_string_builtins(vm);
    json::register_json_builtins(vm);
    json_stream::register_json_stream_builtins(vm);
    xml::register_xml_builtins(vm);
    datetime::register_datetime_builtins(vm);
    document::register_document_builtins(vm);
    calendar::register_calendar_builtins(vm);
    cron::register_cron_builtins(vm);
    regex::register_regex_builtins(vm);
    bytes::register_bytes_builtins(vm);
    compression::register_compression_builtins(vm);
    command_policy::register_command_policy_builtins(vm);
    runtime_scope::register_runtime_scope_builtins(vm);
    crypto::register_crypto_builtins(vm);
    csv::register_csv_builtins(vm);
    junit::register_junit_builtins(vm);
    multipart::register_multipart_builtins(vm);
    url_parse::register_url_builtins(vm);
    web::register_web_builtins(vm);
    cookies::register_cookie_builtins(vm);
    path::register_path_helper_builtins(vm);
    sets::register_set_builtins(vm);
    collections::register_collection_builtins(vm);
    iter::register_iter_builtins(vm);
    event_log::register_event_log_builtins(vm);
    durable_step::register_durable_step_builtins(vm);
    channels::register_channel_builtins(vm);
    channel_guardrails::register_channel_guardrail_builtins(vm);
    shapes::register_shape_builtins(vm);
    testing::register_testing_builtins(vm);
    flow::register_flow_builtins(vm);
    lifecycle_receipts::register_lifecycle_receipt_builtins(vm);
    net_policy::register_net_policy_builtins(vm);
    http_response::register_http_response_builtins(vm);
}

/// Register I/O builtins (requires OS access).
pub fn register_io_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    host::register_host_builtins(vm);
    fs::register_fs_builtins(vm);
    package_snapshot::register_package_snapshot_builtins(vm);
    files::register_file_builtins(vm);
    git::register_git_builtins(vm);
    vision::register_vision_builtins(vm);
    agent_state::register_agent_state_builtins(vm);
    memory::register_memory_builtins(vm);
    session_store::register_session_store_builtins(vm);
    net::register_net_builtins(vm);
    process::register_process_builtins(vm);
    process::register_path_builtins(vm);
    sandbox::register_sandbox_builtins(vm);
    // Clock builtins overlay process::timestamp/elapsed so they honor
    // mock_time / advance_time. Register AFTER process to take precedence.
    clock::register_clock_builtins(vm);
    crate::durable_rate_limit::register_durable_rate_limit_builtins(vm);
    testbench::register_testbench_builtins(vm);
    project::register_project_builtins(vm);
    grounding::register_grounding_builtins(vm);
    tracing::register_tracing_builtins(vm);
    observability::register_observability_builtins(vm);
    timing::register_timing_builtins(vm);
    tui::register_tui_builtins(vm);
}

fn register_agent_stdlib_before_llm(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
    connectors::register_connector_builtins(vm);
    review::register_review_builtins(vm);
    secret_scan::register_secret_scan_builtins(vm);
    tools::register_tool_builtins(vm);
    tool_hooks::register_tool_hooks_builtins(vm);
    crate::composition::register_composition_builtins(vm);
    skills::register_skill_builtins(vm);
    agents_daemon::register_daemon_builtins(vm);
    triggers_stdlib::register_trigger_builtins(vm);
    #[cfg(feature = "postgres")]
    postgres::register_postgres_builtins(vm);
    #[cfg(feature = "sqlite")]
    sqlite::register_sqlite_builtins(vm);
    waitpoints::register_waitpoint_builtins(vm);
    monitors::register_monitor_builtins(vm);
    hitl::register_hitl_builtins(vm);
    hitl_read::register_hitl_read_builtins(vm);
    waitpoint::register_waitpoint_builtins(vm);
    supervisor::register_supervisor_builtins(vm);
    agents::register_agent_builtins(vm);
    pool::register_pool_builtins(vm);
    oauth_storage::register_oauth_storage_builtins(vm);
    oauth_dynreg::register_oauth_dynreg_builtins(vm);
    token_redaction::register_token_redaction_builtins(vm);
    agent_sessions::register_agent_session_builtins(vm);
    artifact_emit::register_artifact_emit_builtins(vm);
    external_agent::register_external_agent_builtins(vm);
    path_scope_guard::register_path_scope_guard_builtins(vm);
    workflow_messages::register_workflow_message_builtins(vm);
    transcript_compact::register_transcript_compaction_builtins(vm);
    compaction::register_compaction_builtins(vm);
    transcript_project::register_transcript_projection_builtins(vm);
    assemble::register_assemble_context_builtin(vm);
    crate::egress::register_egress_builtins(vm);
    crate::security::register_security_builtins(vm);
    register_http_builtins(vm);
    jsonrpc::register_jsonrpc_builtins(vm);
}

fn register_agent_stdlib_after_llm(vm: &mut Vm) {
    register_mcp_builtins(vm);
    register_mcp_server_builtins(vm);
    crate::step_runtime::register_step_builtins(vm);
}

/// Register agent builtins (requires network access and async runtime).
pub fn register_agent_stdlib(vm: &mut Vm) {
    register_agent_stdlib_before_llm(vm);
    register_llm_builtins(vm);
    register_agent_stdlib_after_llm(vm);
}

/// Register all standard builtins on a VM (core + io + agent). Also
/// installs the macro-emitted signature slice into the parser registry
/// (idempotent under repeat calls with the same slice pointer).
pub fn register_vm_stdlib(vm: &mut Vm) {
    register_core_stdlib(vm);
    register_io_stdlib(vm);
    register_agent_stdlib(vm);
    if vm.global("harness").is_none() {
        vm.set_harness(crate::harness::Harness::real());
    }
    harn_builtin_registry::install_builtin_signatures(all_builtin_signatures());
}

pub(crate) fn rebind_execution_state_builtins(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
}

fn stdlib_probe_vm() -> Vm {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    // Name-only/metadata introspection never accesses this path, but passing
    // a real per-platform temp dir keeps registration logic honest if a
    // callee someday validates its parent.
    let tmp = std::env::temp_dir();
    crate::store::register_store_builtins(&mut vm, &tmp);
    crate::checkpoint::register_checkpoint_builtins(&mut vm, &tmp, "default");
    crate::metadata::register_metadata_builtins(&mut vm, &tmp);
    // Install the macro-emitted signatures into the parser registry so any
    // probe-driven name/metadata query (e.g. the alignment test) sees the
    // post-migration sig set. Idempotent under repeat install with the same
    // pointer (which `all_builtin_signatures()` guarantees).
    harn_builtin_registry::install_builtin_signatures(all_builtin_signatures());
    vm
}

/// Aggregate of every `#[harn_builtin]`-emitted `VmBuiltinDef` in the stdlib.
///
/// Backed by the `linkme::distributed_slice` declared on
/// [`crate::stdlib::macros::ALL_BUILTIN_DEFS`] — every annotated fn
/// contributes one entry automatically at link time. Keep builtin registration
/// on this distributed slice instead of per-module arrays plus a central
/// hand-maintained aggregator.
///
/// **Force-link warning** (linkme issue #36): rlib dead-code stripping
/// can drop these statics when `harn-vm` is linked transitively. Every
/// binary that exercises builtins (`harn-cli`, `harn-lsp`, `harn-lint`,
/// `harn-serve`, `harn-dap`) calls [`force_link`] near `main()` to defeat
/// the stripping. The alignment test
/// `linkme_distributed_slice_populates_with_all_builtins` catches a silent
/// regression by asserting the slice is non-empty.
pub fn all_builtin_defs() -> &'static [&'static macros::VmBuiltinDef] {
    &macros::ALL_BUILTIN_DEFS
}

/// Force-link entry point: a `pub fn` that touches `ALL_BUILTIN_DEFS` so
/// the linker keeps every `#[harn_builtin]`-emitted static. Drivers
/// (`harn-cli`, `harn-lsp`, etc.) call this once at startup. Doing nothing
/// at runtime is fine — the side effect is purely a link-time signal.
///
/// See [`linkme issue #36`](https://github.com/dtolnay/linkme/issues/36)
/// for why the explicit touch is necessary on every supported target.
pub fn force_link() {
    // `black_box` prevents LLVM from constant-folding the length read away.
    // The `>= 1` guard never trips at runtime but is a load-bearing safety
    // net: it converts a silent slice-empty regression into a panic that
    // surfaces at the first builtin call instead of a confusing
    // `HARN-NAM-002` somewhere down the line.
    let len = std::hint::black_box(macros::ALL_BUILTIN_DEFS.len());
    assert!(
        len >= 1,
        "linkme distributed_slice ALL_BUILTIN_DEFS is empty — \
         the binary is missing `harn_vm::stdlib::force_link()` at startup, \
         or the linker stripped the harn-vm rlib statics (see linkme issue #36)"
    );
}

/// Driver-facing helper: flatten the macro-emitted `BuiltinDef`s into a
/// `&'static [&'static BuiltinSignature]` slice suitable for
/// [`harn_builtin_registry::install_builtin_signatures`].
///
/// Aliases are expanded into their own `BuiltinSignature` entries (the
/// allocation is leaked once at startup — process-lifetime is appropriate
/// for a global registry).
pub fn all_builtin_signatures() -> &'static [&'static harn_builtin_meta::BuiltinSignature] {
    use std::sync::OnceLock;
    static AGG: OnceLock<Vec<&'static harn_builtin_meta::BuiltinSignature>> = OnceLock::new();
    AGG.get_or_init(|| {
        let mut out: Vec<&'static harn_builtin_meta::BuiltinSignature> = Vec::new();
        for def in all_builtin_defs() {
            if def.runtime_only {
                continue;
            }
            out.push(&def.sig);
            for alias in def.aliases {
                let aliased = harn_builtin_meta::BuiltinSignature {
                    name: alias,
                    ..def.sig
                };
                out.push(Box::leak(Box::new(aliased)));
            }
        }
        out
    })
    .as_slice()
}

/// Register every `#[harn_builtin]`-emitted def on the given VM. Drivers
/// that build the full stdlib via `register_vm_stdlib` get this for free —
/// each module's `register_*_builtins` walks its `MODULE_BUILTINS` slice.
/// This helper is exposed for embedders / tests that want a one-call entry.
pub fn register_all_macro_builtins(vm: &mut Vm) {
    for def in all_builtin_defs() {
        vm.register_builtin_def(def);
    }
}

/// Return the canonical list of all stdlib builtin names. Used by
/// harn-lint and harn-lsp to avoid hardcoded duplicate lists.
pub fn stdlib_builtin_names() -> Vec<String> {
    let vm = stdlib_probe_vm();
    let mut names = vm.builtin_names();
    // Special opcodes/keywords, not registered builtins, but linter
    // should recognize them as valid function calls.
    for extra in [
        "spawn",
        "await",
        "cancel",
        "cancel_graceful",
        "__signal_interrupted",
        "__signal_off_interrupt",
        "__signal_on_interrupt",
        "__signal_raise",
        "is_cancelled",
    ] {
        names.push(extra.to_string());
    }
    names
}

/// Return discoverable metadata for registered stdlib builtins.
pub fn stdlib_builtin_metadata() -> Vec<crate::vm::VmBuiltinMetadata> {
    stdlib_probe_vm().builtin_metadata()
}

/// Reset thread-local stdlib state. Call between test runs.
///
/// Note: `long_running::reset_state()` is intentionally NOT called here
/// because that store is process-global, not thread-local. Wiping it
/// from a per-test reset hook lets one test cancel another test's
/// in-flight worker thread (and lose its `agent_inbox::push`
/// notification), which surfaces as `walk_dir_long_running` /
/// `glob_long_running` timing out under parallel test load. The two
/// call sites that genuinely need a clean handle store —
/// `stdlib::fs::tests::{walk_dir_long_running,glob_long_running}` — call
/// `long_running::reset_state()` explicitly while holding
/// `LONG_RUNNING_TEST_LOCK`.
pub fn reset_stdlib_state() {
    logging::reset_logging_state();
    process::reset_process_state();
    clock::reset_clock_state();
    io::reset_io_state();
    sandbox::reset_sandbox_state();
    git::reset_git_state();
    fs::reset_fs_state();
    json::reset_json_state();
    json_stream::reset_json_stream_state();
    host::reset_host_state();
    host::reset_scoped_host_state();
    observability::reset_observability_state();
    timing::reset_timing_state();
    durable_step::reset_durable_step_state();
    crate::egress::reset_egress_policy_for_host();
    hitl::reset_hitl_state();
    crate::http::reset_http_state();
    crate::external_agent::reset_external_agent_state();
    jsonrpc::reset_jsonrpc_state();
    monitors::reset_monitor_state();
    waitpoints::reset_waitpoint_state();
    waitpoint::reset_waitpoint_state();
    triggers_stdlib::reset_auto_resume_timeouts();
    compaction::reset_compaction_state();
    agents::reset_agent_worker_state();
    agents::workflow::reset_workflow_run_states();
    pool::reset_pool_state();
    #[cfg(feature = "postgres")]
    postgres::reset_postgres_state();
    #[cfg(feature = "sqlite")]
    sqlite::reset_sqlite_state();
    supervisor::reset_supervisor_state();
    agents::records::reset_eval_metrics();
    agents::records::reset_friction_events();
    tools::clear_current_tool_registry();
    tools::clear_tool_synthesis_cache();
    vision::reset_vision_state();
    crate::skills::clear_current_skill_registry();
    template::reset_prompt_registry();
    crate::triggers::clear_webhook_intake_state();
    crate::llm::cache::reset_in_process_cache_state();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn register_vm_stdlib_installs_default_harness_handle() {
        let chunk = crate::compile_source(
            r"
fn __probe_global_harness_clock() {
  const now = harness.clock.now_ms()
  return now >= 0
}

fn main(harness: Harness) {
  return __probe_global_harness_clock()
}
",
        )
        .expect("compile harness clock probe");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);

        assert!(vm.global("harness").is_some());
        let result = vm
            .execute(&chunk)
            .await
            .expect("execute harness clock probe");
        assert!(matches!(result, crate::value::VmValue::Bool(true)));
    }

    /// `harn_stdlib::builtin_reexports` names builtins from a crate that
    /// cannot see the builtin registry, so nothing there can catch a typo or a
    /// rename. This is that check: every re-exported name must resolve to a
    /// real builtin, or `import { … } from "std/…"` binds a reference to
    /// nothing and fails at the call site instead of the import.
    #[test]
    fn every_stdlib_builtin_reexport_names_a_registered_builtin() {
        let registered: std::collections::HashSet<&str> = all_builtin_defs()
            .iter()
            .flat_map(|def| std::iter::once(def.sig.name).chain(def.aliases.iter().copied()))
            .collect();

        let mut checked = 0;
        for entry in harn_stdlib::STDLIB_SOURCES {
            for name in harn_stdlib::builtin_reexports(entry.module) {
                assert!(
                    registered.contains(name),
                    "std/{} re-exports '{name}', which is not a registered builtin",
                    entry.module
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no re-exports were checked — the table or the module list is not being read"
        );
    }
}
