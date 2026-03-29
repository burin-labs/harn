#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

pub mod bridge;
pub mod bridge_builtins;
mod chunk;
mod compiler;
mod http;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod metadata;
pub mod stdlib;
pub mod stdlib_modules;
pub mod store;
pub mod value;
mod vm;

pub use chunk::*;
pub use compiler::*;
pub use http::{register_http_builtins, reset_http_state};
pub use llm::register_llm_builtins;
pub use mcp::{connect_mcp_server, register_mcp_builtins};
pub use metadata::{register_metadata_builtins, register_scan_builtins};
pub use stdlib::register_vm_stdlib;
pub use store::register_store_builtins;
pub use value::*;
pub use vm::*;

/// Reset all thread-local state that can leak between test runs.
/// Call this before each test execution for proper isolation.
pub fn reset_thread_local_state() {
    llm::reset_llm_state();
    http::reset_http_state();
    stdlib::reset_stdlib_state();
}
