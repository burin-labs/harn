//! Standard library builtins for the Harn VM.
//!
//! Each category of builtins lives in its own sub-module.

mod agents;
mod concurrency;
mod crypto;
mod datetime;
mod fs;
mod host;
mod io;
mod json;
mod logging;
mod math;
pub mod process;
mod regex;
mod sets;
mod shapes;
mod strings;
mod testing;
mod tools;
mod tracing;
mod types;

use crate::http::register_http_builtins;
use crate::llm::register_llm_builtins;
use crate::mcp::register_mcp_builtins;
use crate::mcp_server::register_mcp_server_builtins;
use crate::vm::Vm;

// Re-export helpers used by other modules in harn-vm
pub(crate) use json::json_to_vm_value;
pub(crate) use json::schema_result_value;
pub(crate) fn set_thread_source_dir(dir: &std::path::Path) {
    process::set_thread_source_dir(dir);
}

/// Register core builtins: types, math, strings, json, datetime, regex, crypto,
/// sets, shapes, testing. These are pure/deterministic and require no I/O.
pub fn register_core_stdlib(vm: &mut Vm) {
    types::register_type_builtins(vm);
    math::register_math_builtins(vm);
    strings::register_string_builtins(vm);
    json::register_json_builtins(vm);
    datetime::register_datetime_builtins(vm);
    regex::register_regex_builtins(vm);
    crypto::register_crypto_builtins(vm);
    sets::register_set_builtins(vm);
    shapes::register_shape_builtins(vm);
    testing::register_testing_builtins(vm);
}

/// Register I/O builtins: filesystem, process, logging, tracing, I/O.
/// Requires OS access (file reads, process spawning, environment vars).
pub fn register_io_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    host::register_host_builtins(vm);
    fs::register_fs_builtins(vm);
    process::register_process_builtins(vm);
    process::register_path_builtins(vm);
    tracing::register_tracing_builtins(vm);
}

/// Register agent builtins: concurrency, tools, agents, HTTP, LLM, MCP.
/// Requires network access and async runtime.
pub fn register_agent_stdlib(vm: &mut Vm) {
    concurrency::register_concurrency_builtins(vm);
    tools::register_tool_builtins(vm);
    agents::register_agent_builtins(vm);
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

/// Return the canonical list of all stdlib builtin names.
/// This creates a temporary VM, registers all builtins, and collects the names.
/// Used by harn-lint and harn-lsp to avoid hardcoded duplicate lists.
pub fn stdlib_builtin_names() -> Vec<String> {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    // Register path-dependent builtins with a dummy path so we capture their names.
    let tmp = std::path::PathBuf::from("/tmp");
    crate::store::register_store_builtins(&mut vm, &tmp);
    crate::checkpoint::register_checkpoint_builtins(&mut vm, &tmp, "default");
    crate::metadata::register_metadata_builtins(&mut vm, &tmp);
    crate::metadata::register_scan_builtins(&mut vm, &tmp);
    let mut names = vm.builtin_names();
    // These are handled as special opcodes/keywords, not registered builtins,
    // but the linter should recognize them as valid function calls.
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

/// Reset thread-local stdlib state (logging, tracing, source dir). Call between test runs.
pub fn reset_stdlib_state() {
    logging::reset_logging_state();
    process::reset_process_state();
    fs::reset_fs_state();
    json::reset_json_state();
    host::reset_host_state();
}
