use crate::value::{VmError, VmValue};

use super::defs::McpToolDef;

/// Executable MCP handlers bound to one immutable prepared catalog.
pub struct McpToolSet {
    definitions: Vec<McpToolDef>,
    prepared: crate::tool_registry::PreparedToolCatalog,
}

impl McpToolSet {
    /// Validate tool definitions and compile their executable schemas once.
    pub fn prepare(
        definitions: Vec<McpToolDef>,
    ) -> Result<Self, crate::tool_registry::PreparedToolCatalogError> {
        let definitions = definitions
            .into_iter()
            .filter(|tool| {
                tool.catalog
                    .governance
                    .allows(crate::tool_registry::ToolAudience::Mcp)
            })
            .collect::<Vec<_>>();
        let catalog = crate::tool_registry::ToolCatalog {
            schema_version: crate::tool_registry::ToolCatalogSchemaVersion::V1,
            info: None,
            cli: None,
            tools: definitions
                .iter()
                .map(|tool| tool.catalog.clone())
                .collect(),
            components: None,
        };
        let prepared = crate::tool_registry::PreparedToolCatalog::prepare(catalog)?;
        Ok(Self {
            definitions,
            prepared,
        })
    }

    pub(crate) fn prepared(&self) -> &crate::tool_registry::PreparedToolCatalog {
        &self.prepared
    }
}

impl std::ops::Deref for McpToolSet {
    type Target = [McpToolDef];

    fn deref(&self) -> &Self::Target {
        &self.definitions
    }
}

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<McpToolSet, VmError> {
    let catalog = crate::tool_registry::tool_registry_catalog_for_audience(
        registry,
        crate::tool_registry::ToolAudience::Mcp,
    )?;
    let prepared = crate::tool_registry::PreparedToolCatalog::prepare(catalog)
        .map_err(|error| VmError::Runtime(format!("invalid executable tool catalog: {error}")))?;
    let definitions = crate::tool_registry::executable_tools_for_audience(
        registry,
        crate::tool_registry::ToolAudience::Mcp,
    )?
    .into_iter()
    .map(|tool| McpToolDef {
        catalog: tool.catalog,
        handler: tool.handler,
    })
    .collect::<Vec<_>>();
    if definitions.len() != prepared.catalog().tools.len() {
        return Err(VmError::Runtime(
            "executable MCP handlers do not match the prepared tool catalog".into(),
        ));
    }
    Ok(McpToolSet {
        definitions,
        prepared,
    })
}

#[cfg(test)]
pub(super) fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    crate::tool_registry::params_to_json_schema(params).expect("test parameter schema")
}
