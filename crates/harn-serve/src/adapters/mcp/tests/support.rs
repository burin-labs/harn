//! Read-only views of a server for assertions on what it serves.
use super::super::*;

impl McpServer {
    pub(super) fn exports_tool_catalog(&self) -> &harn_vm::tool_registry::ToolCatalog {
        match &self.source {
            McpSource::Exports(exports) => &exports.tool_catalog,
            McpSource::Script(_) => panic!("expected an exports server"),
        }
    }

    pub(super) fn tools_list_result(&self, params: &JsonValue) -> JsonValue {
        mcp_protocol::list_response(
            json!(0),
            "tools/list",
            "tools",
            params,
            self.source.tool_entries(),
        )["result"]
            .clone()
    }

    pub(super) fn resources_list_result(&self, params: &JsonValue) -> JsonValue {
        mcp_protocol::list_response(
            json!(0),
            "resources/list",
            "resources",
            params,
            self.resource_entries(),
        )["result"]
            .clone()
    }
}
