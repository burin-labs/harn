use crate::value::{VmError, VmValue};

use super::defs::McpToolDef;

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<Vec<McpToolDef>, VmError> {
    crate::tool_registry::executable_tools_for_audience(
        registry,
        crate::tool_registry::ToolAudience::Mcp,
    )?
    .into_iter()
    .map(|tool| {
        Ok(McpToolDef {
            catalog: tool.catalog,
            handler: tool.handler,
        })
    })
    .collect()
}

#[cfg(test)]
pub(super) fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    crate::tool_registry::params_to_json_schema(params).expect("test parameter schema")
}
