#![allow(clippy::result_large_err, clippy::cloned_ref_to_slice_refs)]

pub mod agent_events;
pub mod agent_sessions;
pub mod bridge;
pub mod checkpoint;
mod chunk;
mod compiler;
pub mod events;
mod http;
pub mod jsonrpc;
pub mod llm;
pub mod llm_config;
pub mod mcp;
pub mod mcp_card;
pub mod mcp_registry;
pub mod mcp_server;
pub mod metadata;
pub mod orchestration;
pub mod runtime_paths;
pub mod schema;
pub mod skills;
pub mod stdlib;
pub mod stdlib_modules;
pub mod store;
pub mod tool_annotations;
pub mod tracing;
pub mod value;
pub mod visible_text;
mod vm;
pub mod workspace_path;

pub use checkpoint::register_checkpoint_builtins;
pub use chunk::*;
pub use compiler::*;
pub use http::{register_http_builtins, reset_http_state};
pub use llm::register_llm_builtins;
pub use mcp::{
    connect_mcp_server, connect_mcp_server_from_json, connect_mcp_server_from_spec,
    register_mcp_builtins,
};
pub use mcp_card::{fetch_server_card, load_server_card_from_path, CardError};
pub use mcp_registry::{
    active_handle as mcp_active_handle, ensure_active as mcp_ensure_active,
    get_registration as mcp_get_registration, install_active as mcp_install_active,
    is_registered as mcp_is_registered, register_servers as mcp_register_servers,
    release as mcp_release, reset as mcp_reset_registry, snapshot_status as mcp_snapshot_status,
    sweep_expired as mcp_sweep_expired, RegisteredMcpServer, RegistryStatus,
};
pub use mcp_server::{
    take_mcp_serve_prompts, take_mcp_serve_registry, take_mcp_serve_resource_templates,
    take_mcp_serve_resources, tool_registry_to_mcp_tools, McpServer,
};
pub use metadata::{register_metadata_builtins, register_scan_builtins};
pub use stdlib::host::{clear_host_call_bridge, set_host_call_bridge, HostCallBridge};
pub use stdlib::template::{
    lookup_prompt_consumers, lookup_prompt_span, prompt_render_indices, record_prompt_render_index,
    PromptSourceSpan, PromptSpanKind,
};
pub use stdlib::{
    register_agent_stdlib, register_core_stdlib, register_io_stdlib, register_vm_stdlib,
};
pub use store::register_store_builtins;
pub use value::*;
pub use vm::*;

/// Lex, parse, type-check, and compile source to bytecode in one call.
/// Bails on the first type error. For callers that need diagnostics
/// rather than early exit, use `harn_parser::check_source` directly
/// and then call `Compiler::new().compile(&program)`.
pub fn compile_source(source: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    Compiler::new().compile(&program).map_err(|e| e.to_string())
}

/// Reset all thread-local state that can leak between test runs.
pub fn reset_thread_local_state() {
    llm::reset_llm_state();
    llm_config::clear_user_overrides();
    http::reset_http_state();
    stdlib::reset_stdlib_state();
    orchestration::clear_runtime_hooks();
    events::reset_event_sinks();
    agent_events::reset_all_sinks();
    agent_sessions::reset_session_store();
    mcp_registry::reset();
}
