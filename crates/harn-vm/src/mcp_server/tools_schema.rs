use crate::value::{VmError, VmValue};

use super::defs::McpToolDef;

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<Vec<McpToolDef>, VmError> {
    crate::tool_registry::executable_tools(registry)?
        .into_iter()
        .map(|tool| {
            let task_support = tool
                .catalog
                .execution
                .as_ref()
                .and_then(|execution| execution.get("taskSupport"))
                .and_then(serde_json::Value::as_str)
                .map(crate::mcp_tasks::McpTaskSupport::from_wire)
                .unwrap_or_default();
            Ok(McpToolDef {
                name: tool.catalog.name,
                title: tool.catalog.title,
                description: tool.catalog.description.unwrap_or_default(),
                input_schema: tool.catalog.input_schema,
                output_schema: tool.catalog.output_schema,
                annotations: tool.catalog.annotations,
                icons: tool.catalog.icons,
                meta: tool.catalog.meta,
                task_support,
                handler: tool.handler,
            })
        })
        .collect()
}

#[cfg(test)]
pub(super) fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    crate::tool_registry::params_to_json_schema(params).expect("test parameter schema")
}
