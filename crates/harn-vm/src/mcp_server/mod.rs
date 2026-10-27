//! Script-published MCP surface: tools, resources, resource templates, and
//! prompts a Harn pipeline registers for `harn serve mcp`.
//!
//! This is the mirror of `mcp.rs` (the client). A pipeline registers
//! capabilities with `mcp_tools()`, `mcp_resource()`, `mcp_resource_template()`,
//! and `mcp_prompt()`; [`McpScriptRegistry`] holds them with their handlers.
//! The protocol server that answers MCP clients lives in `harn-serve`, which
//! serves this registry and exported `pub fn`s through the same session,
//! authorization, and task handling.

pub(crate) mod convert;
mod defs;
mod register;
mod registry;
#[cfg(test)]
pub(crate) use registry::scope_composition_for_test;
pub(crate) use registry::{composition_allows_tool, is_served_composition};
mod tools_schema;
pub mod uri;

#[cfg(test)]
mod tests;

pub use defs::{
    McpCompletionSource, McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef,
    McpServerMetadata, McpToolDef,
};
pub use register::{
    register_mcp_server_builtins, reset_mcp_serve_publication, take_mcp_serve_metadata,
    take_mcp_serve_prompts, take_mcp_serve_registry, take_mcp_serve_rejection,
    take_mcp_serve_resource_templates, take_mcp_serve_resources,
};
pub use registry::{McpScriptRegistry, McpScriptSurface};
pub use tools_schema::{tool_registry_to_mcp_tools, McpToolSet};
