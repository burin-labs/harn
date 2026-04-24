//! Standard library builtins for the Harn VM.

mod agent_sessions;
pub mod agent_state;
mod agents;
mod agents_daemon;
pub(crate) mod assemble;
mod bytes;
mod concurrency;
mod connectors;
mod crypto;
mod datetime;
mod fs;
pub(crate) mod hitl;
mod hitl_read;
pub mod host;
mod io;
mod iter;
pub(crate) mod json;
mod logging;
mod math;
mod monitors;
mod path;
pub mod process;
mod project;
mod project_catalog;
mod project_enrich;
mod regex;
mod review;
pub(crate) mod sandbox;
pub mod secret_scan;
mod sets;
mod shapes;
mod skills;
mod strings;
pub mod template;
mod testing;
pub(crate) mod tools;
pub mod tracing;
mod transcript_compact;
mod triggers_stdlib;
mod types;
mod vision;
pub(crate) mod waitpoint;
mod waitpoints;
pub mod workflow_messages;

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
    datetime::register_datetime_builtins(vm);
    regex::register_regex_builtins(vm);
    bytes::register_bytes_builtins(vm);
    crypto::register_crypto_builtins(vm);
    path::register_path_helper_builtins(vm);
    sets::register_set_builtins(vm);
    iter::register_iter_builtins(vm);
    shapes::register_shape_builtins(vm);
    testing::register_testing_builtins(vm);
}

/// Register I/O builtins (requires OS access).
pub fn register_io_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    host::register_host_builtins(vm);
    fs::register_fs_builtins(vm);
    vision::register_vision_builtins(vm);
    agent_state::register_agent_state_builtins(vm);
    process::register_process_builtins(vm);
    process::register_path_builtins(vm);
    project::register_project_builtins(vm);
    tracing::register_tracing_builtins(vm);
}

/// Register agent builtins (requires network access and async runtime).
pub fn register_agent_stdlib(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
    connectors::register_connector_builtins(vm);
    review::register_review_builtins(vm);
    secret_scan::register_secret_scan_builtins(vm);
    tools::register_tool_builtins(vm);
    skills::register_skill_builtins(vm);
    agents_daemon::register_daemon_builtins(vm);
    triggers_stdlib::register_trigger_builtins(vm);
    waitpoints::register_waitpoint_builtins(vm);
    monitors::register_monitor_builtins(vm);
    hitl::register_hitl_builtins(vm);
    hitl_read::register_hitl_read_builtins(vm);
    waitpoint::register_waitpoint_builtins(vm);
    agents::register_agent_builtins(vm);
    agent_sessions::register_agent_session_builtins(vm);
    workflow_messages::register_workflow_message_builtins(vm);
    transcript_compact::register_transcript_compaction_builtins(vm);
    assemble::register_assemble_context_builtin(vm);
    register_http_builtins(vm);
    register_llm_builtins(vm);
    register_mcp_builtins(vm);
    register_mcp_server_builtins(vm);
}

/// Register all standard builtins on a VM (core + io + agent).
pub fn register_vm_stdlib(vm: &mut Vm) {
    register_core_stdlib(vm);
    register_io_stdlib(vm);
    register_agent_stdlib(vm);
}

/// Return the canonical list of all stdlib builtin names. Used by
/// harn-lint and harn-lsp to avoid hardcoded duplicate lists.
pub fn stdlib_builtin_names() -> Vec<String> {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    // Name-only introspection — the path is never accessed, but passing
    // a real per-platform temp dir keeps the registration logic honest
    // when the callee someday decides it needs a valid parent.
    let tmp = std::env::temp_dir();
    crate::store::register_store_builtins(&mut vm, &tmp);
    crate::checkpoint::register_checkpoint_builtins(&mut vm, &tmp, "default");
    crate::metadata::register_metadata_builtins(&mut vm, &tmp);
    crate::metadata::register_scan_builtins(&mut vm);
    let mut names = vm.builtin_names();
    // Special opcodes/keywords, not registered builtins, but linter
    // should recognize them as valid function calls.
    for extra in [
        "spawn",
        "await",
        "cancel",
        "cancel_graceful",
        "is_cancelled",
    ] {
        names.push(extra.to_string());
    }
    names
}

/// Reset thread-local stdlib state. Call between test runs.
pub fn reset_stdlib_state() {
    logging::reset_logging_state();
    process::reset_process_state();
    sandbox::reset_sandbox_state();
    fs::reset_fs_state();
    json::reset_json_state();
    host::reset_host_state();
    hitl::reset_hitl_state();
    monitors::reset_monitor_state();
    waitpoints::reset_waitpoint_state();
    waitpoint::reset_waitpoint_state();
    agents::records::reset_eval_metrics();
    tools::clear_current_tool_registry();
    vision::reset_vision_state();
    crate::skills::clear_current_skill_registry();
    template::reset_prompt_registry();
}
