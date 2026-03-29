//! Standard library builtins for the Harn VM.
//!
//! Each category of builtins lives in its own sub-module.

mod concurrency;
mod crypto;
mod datetime;
mod fs;
mod io;
mod json;
mod logging;
mod math;
mod process;
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
use crate::vm::Vm;

// Re-export helpers used by other modules in harn-vm
pub(crate) use json::json_to_vm_value;

/// Register all standard builtins on a VM.
pub fn register_vm_stdlib(vm: &mut Vm) {
    io::register_io_builtins(vm);
    types::register_type_builtins(vm);
    math::register_math_builtins(vm);
    strings::register_string_builtins(vm);
    json::register_json_builtins(vm);
    fs::register_fs_builtins(vm);
    process::register_process_builtins(vm);
    datetime::register_datetime_builtins(vm);
    regex::register_regex_builtins(vm);
    crypto::register_crypto_builtins(vm);
    sets::register_set_builtins(vm);
    testing::register_testing_builtins(vm);
    concurrency::register_concurrency_builtins(vm);
    tools::register_tool_builtins(vm);
    tracing::register_tracing_builtins(vm);
    shapes::register_shape_builtins(vm);

    register_http_builtins(vm);
    register_llm_builtins(vm);
    register_mcp_builtins(vm);
}

/// Reset thread-local stdlib state (logging, tracing). Call between test runs.
pub fn reset_stdlib_state() {
    logging::reset_logging_state();
}
