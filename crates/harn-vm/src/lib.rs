#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

pub mod bridge;
pub mod checkpoint;
mod chunk;
mod compiler;
pub mod events;
mod http;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod mcp_server;
pub mod metadata;
pub mod orchestration;
pub mod runtime_paths;
pub mod schema;
pub mod stdlib;
pub mod stdlib_modules;
pub mod store;
pub mod tracing;
pub mod value;
mod vm;

pub use checkpoint::register_checkpoint_builtins;
pub use chunk::*;
pub use compiler::*;
pub use http::{register_http_builtins, reset_http_state};
pub use llm::register_llm_builtins;
pub use mcp::{
    connect_mcp_server, connect_mcp_server_from_json, connect_mcp_server_from_spec,
    register_mcp_builtins,
};
pub use mcp_server::{
    take_mcp_serve_prompts, take_mcp_serve_registry, take_mcp_serve_resource_templates,
    take_mcp_serve_resources, tool_registry_to_mcp_tools, McpServer,
};
pub use metadata::{register_metadata_builtins, register_scan_builtins};
pub use stdlib::{
    register_agent_stdlib, register_core_stdlib, register_io_stdlib, register_vm_stdlib,
};
pub use store::register_store_builtins;
pub use value::*;
pub use vm::*;

/// Reset all thread-local state that can leak between test runs.
/// Call this before each test execution for proper isolation.
pub fn reset_thread_local_state() {
    llm::reset_llm_state();
    http::reset_http_state();
    stdlib::reset_stdlib_state();
    events::reset_event_sinks();
}
