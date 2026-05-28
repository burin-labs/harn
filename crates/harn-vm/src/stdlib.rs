//! Standard library builtins for the Harn VM.

pub mod macros;

mod agent_sessions;
pub mod agent_state;
pub(crate) mod agents;
mod agents_daemon;
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
mod crypto;
mod csv;
mod datetime;
mod durable_step;
mod event_log;
pub(crate) mod files;
mod flow;
mod fs;
mod git;
pub(crate) mod harn_entry;
pub(crate) mod hitl;
mod hitl_read;
pub mod host;
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
mod net_policy;
mod oauth_dynreg;
mod oauth_storage;
mod observability;
mod options;
mod path;
pub(crate) mod path_scope_guard;
pub(crate) mod pool;
mod postgres;
pub mod process;
mod project;
mod project_catalog;
mod project_enrich;
mod regex;
pub(crate) mod registration;
mod review;
mod runtime_scope;
pub(crate) mod sandbox;
pub mod secret_scan;
mod sets;
pub(crate) mod shapes;
mod skills;
mod strings;
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
mod xml;

use crate::http::register_http_builtins;
use crate::llm::{register_deferred_llm_builtins, register_llm_builtins};
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
    calendar::register_calendar_builtins(vm);
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
}

/// Register I/O builtins (requires OS access).
pub fn register_io_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    host::register_host_builtins(vm);
    fs::register_fs_builtins(vm);
    files::register_file_builtins(vm);
    git::register_git_builtins(vm);
    vision::register_vision_builtins(vm);
    agent_state::register_agent_state_builtins(vm);
    memory::register_memory_builtins(vm);
    process::register_process_builtins(vm);
    process::register_path_builtins(vm);
    sandbox::register_sandbox_builtins(vm);
    // Clock builtins overlay process::timestamp/elapsed so they honor
    // mock_time / advance_time. Register AFTER process to take precedence.
    clock::register_clock_builtins(vm);
    testbench::register_testbench_builtins(vm);
    project::register_project_builtins(vm);
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
    postgres::register_postgres_builtins(vm);
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
    path_scope_guard::register_path_scope_guard_builtins(vm);
    workflow_messages::register_workflow_message_builtins(vm);
    transcript_compact::register_transcript_compaction_builtins(vm);
    compaction::register_compaction_builtins(vm);
    transcript_project::register_transcript_projection_builtins(vm);
    assemble::register_assemble_context_builtin(vm);
    crate::egress::register_egress_builtins(vm);
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

fn register_agent_stdlib_with_deferred_llm(vm: &mut Vm) {
    register_agent_stdlib_before_llm(vm);
    register_deferred_llm_builtins(vm);
    register_agent_stdlib_after_llm(vm);
}

/// Register all standard builtins on a VM (core + io + agent). Also
/// installs the macro-emitted signature slice into the parser registry
/// (idempotent under repeat calls with the same slice pointer).
pub fn register_vm_stdlib(vm: &mut Vm) {
    register_core_stdlib(vm);
    register_io_stdlib(vm);
    register_agent_stdlib(vm);
    harn_builtin_registry::install_builtin_signatures(all_builtin_signatures());
}

/// Register the stdlib shape used by latency-sensitive CLI execution. Also
/// installs the macro-emitted signature slice into the parser registry.
pub fn register_vm_stdlib_with_deferred_llm(vm: &mut Vm) {
    register_core_stdlib(vm);
    register_io_stdlib(vm);
    register_agent_stdlib_with_deferred_llm(vm);
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
    crate::metadata::register_scan_builtins(&mut vm);
    // Install the macro-emitted signatures into the parser registry so any
    // probe-driven name/metadata query (e.g. the alignment test) sees the
    // post-migration sig set. Idempotent under repeat install with the same
    // pointer (which `all_builtin_signatures()` guarantees).
    harn_builtin_registry::install_builtin_signatures(all_builtin_signatures());
    vm
}

/// Aggregate of every `#[harn_builtin]`-emitted `VmBuiltinDef` in the stdlib.
///
/// Each migrated module exposes a `MODULE_BUILTINS: &[&VmBuiltinDef]` slice;
/// they're concatenated here in deterministic alphabetical-by-module order.
/// Returned with `'static` lifetime so the slice can be installed into the
/// parser registry without leaking.
pub fn all_builtin_defs() -> &'static [&'static macros::VmBuiltinDef] {
    use std::sync::OnceLock;
    static AGG: OnceLock<Vec<&'static macros::VmBuiltinDef>> = OnceLock::new();
    AGG.get_or_init(|| {
        // Per-module slices are pushed here as modules migrate to
        // `#[harn_builtin]`. Order is alphabetical by module file name for
        // predictability.
        let mut out: Vec<&'static macros::VmBuiltinDef> = Vec::new();
        out.extend_from_slice(agent_sessions::MODULE_BUILTINS);
        out.extend_from_slice(agent_state::MODULE_BUILTINS);
        out.extend_from_slice(agents_daemon::MODULE_BUILTINS);
        out.extend_from_slice(bytes::MODULE_BUILTINS);
        out.extend_from_slice(calendar::MODULE_BUILTINS);
        out.extend_from_slice(channel_guardrails::MODULE_BUILTINS);
        out.extend_from_slice(clock::MODULE_BUILTINS);
        out.extend_from_slice(collections::MODULE_BUILTINS);
        out.extend_from_slice(command_policy::MODULE_BUILTINS);
        out.extend_from_slice(compaction::MODULE_BUILTINS);
        out.extend_from_slice(compression::MODULE_BUILTINS);
        out.extend_from_slice(connectors::MODULE_BUILTINS);
        out.extend_from_slice(cookies::MODULE_BUILTINS);
        out.extend_from_slice(crypto::MODULE_BUILTINS);
        out.extend_from_slice(csv::MODULE_BUILTINS);
        out.extend_from_slice(datetime::MODULE_BUILTINS);
        out.extend_from_slice(durable_step::MODULE_BUILTINS);
        out.extend_from_slice(event_log::MODULE_BUILTINS);
        out.extend_from_slice(flow::MODULE_BUILTINS);
        out.extend_from_slice(fs::MODULE_BUILTINS);
        out.extend_from_slice(hitl::MODULE_BUILTINS);
        out.extend_from_slice(hitl_read::MODULE_BUILTINS);
        out.extend_from_slice(host::MODULE_BUILTINS);
        out.extend_from_slice(io::MODULE_BUILTINS);
        out.extend_from_slice(iter::MODULE_BUILTINS);
        out.extend_from_slice(json::MODULE_BUILTINS);
        out.extend_from_slice(json_stream::MODULE_BUILTINS);
        out.extend_from_slice(junit::MODULE_BUILTINS);
        out.extend_from_slice(lifecycle_receipts::MODULE_BUILTINS);
        out.extend_from_slice(math::MODULE_BUILTINS);
        out.extend_from_slice(memory::MODULE_BUILTINS);
        out.extend_from_slice(monitors::MODULE_BUILTINS);
        out.extend_from_slice(multipart::MODULE_BUILTINS);
        out.extend_from_slice(net_policy::MODULE_BUILTINS);
        out.extend_from_slice(oauth_dynreg::MODULE_BUILTINS);
        out.extend_from_slice(oauth_storage::MODULE_BUILTINS);
        out.extend_from_slice(observability::MODULE_BUILTINS);
        out.extend_from_slice(path::MODULE_BUILTINS);
        out.extend_from_slice(path_scope_guard::MODULE_BUILTINS);
        out.extend_from_slice(postgres::MODULE_BUILTINS);
        out.extend_from_slice(process::MODULE_BUILTINS);
        out.extend_from_slice(project::MODULE_BUILTINS);
        out.extend_from_slice(agents::records::MODULE_BUILTINS);
        out.extend_from_slice(regex::MODULE_BUILTINS);
        out.extend_from_slice(runtime_scope::MODULE_BUILTINS);
        out.extend_from_slice(sandbox::MODULE_BUILTINS);
        out.extend_from_slice(sets::MODULE_BUILTINS);
        out.extend_from_slice(shapes::MODULE_BUILTINS);
        out.extend_from_slice(skills::MODULE_BUILTINS);
        out.extend_from_slice(strings::MODULE_BUILTINS);
        out.extend_from_slice(supervisor::MODULE_BUILTINS);
        out.extend_from_slice(testbench::MODULE_BUILTINS);
        out.extend_from_slice(testing::MODULE_BUILTINS);
        out.extend_from_slice(timing::MODULE_BUILTINS);
        out.extend_from_slice(tool_hooks::MODULE_BUILTINS);
        out.extend_from_slice(token_redaction::MODULE_BUILTINS);
        out.extend_from_slice(tools::MODULE_BUILTINS);
        out.extend_from_slice(tracing::MODULE_BUILTINS);
        out.extend_from_slice(triggers_stdlib::MODULE_BUILTINS);
        out.extend_from_slice(tui::MODULE_BUILTINS);
        out.extend_from_slice(types::MODULE_BUILTINS);
        out.extend_from_slice(url_parse::MODULE_BUILTINS);
        out.extend_from_slice(waitpoint::MODULE_BUILTINS);
        out.extend_from_slice(waitpoints::MODULE_BUILTINS);
        out.extend_from_slice(web::MODULE_BUILTINS);
        out.extend_from_slice(xml::MODULE_BUILTINS);
        out
    })
    .as_slice()
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
    fs::reset_fs_state();
    json::reset_json_state();
    json_stream::reset_json_stream_state();
    host::reset_host_state();
    observability::reset_observability_state();
    timing::reset_timing_state();
    durable_step::reset_durable_step_state();
    crate::egress::reset_egress_policy_for_host();
    hitl::reset_hitl_state();
    crate::http::reset_http_state();
    jsonrpc::reset_jsonrpc_state();
    monitors::reset_monitor_state();
    waitpoints::reset_waitpoint_state();
    waitpoint::reset_waitpoint_state();
    triggers_stdlib::reset_auto_resume_timeouts();
    compaction::reset_compaction_state();
    agents::reset_agent_worker_state();
    pool::reset_pool_state();
    postgres::reset_postgres_state();
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
