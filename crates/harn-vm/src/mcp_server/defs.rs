use std::collections::BTreeMap;

use crate::value::VmClosure;

/// Script-supplied metadata for a Harn-served MCP endpoint.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct McpServerMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
    pub instructions: Option<String>,
}

/// A tool extracted from a Harn tool_registry, ready to serve over MCP.
pub struct McpToolDef {
    /// The one transport-neutral entry projected by CLI, MCP, documentation,
    /// and generated clients.
    pub catalog: crate::tool_registry::ToolCatalogEntry,
    pub handler: VmClosure,
}

/// A static resource to serve over MCP.
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    /// Protocol extension metadata projected on both discovery and content.
    pub meta: Option<serde_json::Value>,
    pub text: String,
}

/// A parameterized resource template (RFC 6570 URI template).
pub struct McpResourceTemplateDef {
    pub uri_template: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub completions: BTreeMap<String, McpCompletionSource>,
    pub handler: VmClosure,
}

/// Static or computed suggestions for a prompt/resource-template argument.
#[derive(Default)]
pub struct McpCompletionSource {
    pub values: Vec<String>,
    pub handler: Option<VmClosure>,
}

/// A prompt argument definition.
pub struct McpPromptArgDef {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub completion: Option<McpCompletionSource>,
}

/// A prompt template to serve over MCP.
pub struct McpPromptDef {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub arguments: Option<Vec<McpPromptArgDef>>,
    pub handler: VmClosure,
}
