//! MCP server mode: expose Harn tools, resources, resource templates, and
//! prompts as MCP capabilities over stdio.
//!
//! This is the mirror of `mcp.rs` (the client). A Harn pipeline registers
//! capabilities with `mcp_tools()`, `mcp_resource()`, `mcp_resource_template()`,
//! and `mcp_prompt()`, then `harn serve mcp` detects the script-driven
//! surface and starts this server, making them callable by Claude
//! Desktop, Cursor, or any MCP client.

mod convert;
mod defs;
mod register;
mod server;
mod tools_schema;
mod uri;

#[cfg(test)]
mod tests;

use crate::mcp_protocol::PROTOCOL_VERSION;
pub use defs::{
    McpCompletionSource, McpPromptArgDef, McpPromptDef, McpResourceDef, McpResourceTemplateDef,
    McpServerMetadata, McpToolDef,
};
pub use register::{
    register_mcp_server_builtins, take_mcp_serve_metadata, take_mcp_serve_prompts,
    take_mcp_serve_registry, take_mcp_serve_resource_templates, take_mcp_serve_resources,
};
pub use server::McpServer;
pub use tools_schema::tool_registry_to_mcp_tools;
