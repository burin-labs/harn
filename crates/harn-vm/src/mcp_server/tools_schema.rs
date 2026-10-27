use std::collections::BTreeMap;
use std::sync::Arc;

use crate::value::{DictMap, VmError, VmValue};

use super::defs::McpToolDef;

/// Executable MCP handlers bound to one immutable prepared catalog.
pub struct McpToolSet {
    definitions: Vec<McpToolDef>,
    prepared: crate::tool_registry::PreparedToolCatalog,
    published_entries: BTreeMap<String, Arc<DictMap>>,
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
            schema_version: crate::tool_registry::ToolCatalogSchemaVersion::V2,
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
            published_entries: BTreeMap::new(),
        })
    }

    pub(crate) fn prepared(&self) -> &crate::tool_registry::PreparedToolCatalog {
        &self.prepared
    }

    pub(crate) fn published_entries(&self) -> &BTreeMap<String, Arc<DictMap>> {
        &self.published_entries
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
        invocation_requirement: tool.invocation_requirement,
    })
    .collect::<Vec<_>>();
    if definitions.len() != prepared.catalog().tools.len() {
        return Err(VmError::Runtime(
            "executable MCP handlers do not match the prepared tool catalog".into(),
        ));
    }
    let mut published_entries = BTreeMap::new();
    if let Some(VmValue::List(entries)) = registry.as_dict().and_then(|dict| dict.get("tools")) {
        for entry in entries.iter() {
            if let VmValue::Dict(entry) = entry {
                if let Some(VmValue::String(name)) = entry.get("name") {
                    if definitions
                        .iter()
                        .any(|tool| tool.catalog.name.as_str() == name.as_str())
                    {
                        published_entries.insert(name.to_string(), Arc::clone(entry));
                    }
                }
            }
        }
    }
    Ok(McpToolSet {
        definitions,
        prepared,
        published_entries,
    })
}

#[cfg(test)]
pub(super) fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    crate::tool_registry::params_to_json_schema(params).expect("test parameter schema")
}
