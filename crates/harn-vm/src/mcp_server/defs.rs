use crate::value::VmClosure;

/// A tool extracted from a Harn tool_registry, ready to serve over MCP.
pub struct McpToolDef {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    pub annotations: Option<serde_json::Value>,
    pub handler: VmClosure,
}

/// A static resource to serve over MCP.
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub text: String,
}

/// A parameterized resource template (RFC 6570 URI template).
pub struct McpResourceTemplateDef {
    pub uri_template: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub handler: VmClosure,
}

/// A prompt argument definition.
pub struct McpPromptArgDef {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

/// A prompt template to serve over MCP.
pub struct McpPromptDef {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub arguments: Option<Vec<McpPromptArgDef>>,
    pub handler: VmClosure,
}
