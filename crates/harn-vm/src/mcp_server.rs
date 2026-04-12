//! MCP server mode: expose Harn tools, resources, resource templates, and
//! prompts as MCP capabilities over stdio.
//!
//! This is the mirror of `mcp.rs` (the client). A Harn pipeline registers
//! capabilities with `mcp_tools()`, `mcp_resource()`, `mcp_resource_template()`,
//! and `mcp_prompt()`, then the CLI's `mcp-serve` command starts this server,
//! making them callable by Claude Desktop, Cursor, or any MCP client.

use std::cell::RefCell;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::stdlib::json_to_vm_value;
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    /// Stores the tool registry set by `mcp_tools` / `mcp_serve`.
    static MCP_SERVE_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
    /// Static resources registered by `mcp_resource`.
    static MCP_SERVE_RESOURCES: RefCell<Vec<McpResourceDef>> = const { RefCell::new(Vec::new()) };
    /// Resource templates registered by `mcp_resource_template`.
    static MCP_SERVE_RESOURCE_TEMPLATES: RefCell<Vec<McpResourceTemplateDef>> = const { RefCell::new(Vec::new()) };
    /// Prompts registered by `mcp_prompt`.
    static MCP_SERVE_PROMPTS: RefCell<Vec<McpPromptDef>> = const { RefCell::new(Vec::new()) };
}

// =============================================================================
// Builtins
// =============================================================================

/// Register all MCP server builtins on a VM.
pub fn register_mcp_server_builtins(vm: &mut Vm) {
    // ---- tools (renamed from mcp_serve; old name kept as alias) ----
    fn register_tools_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
        let registry = args.first().cloned().ok_or_else(|| {
            VmError::Runtime("mcp_tools: requires a tool_registry argument".into())
        })?;
        if let VmValue::Dict(d) = &registry {
            match d.get("_type") {
                Some(VmValue::String(t)) if &**t == "tool_registry" => {}
                _ => {
                    return Err(VmError::Runtime(
                        "mcp_tools: argument must be a tool registry (created with tool_registry())"
                            .into(),
                    ));
                }
            }
        } else {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry".into(),
            ));
        }
        MCP_SERVE_REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(registry);
        });
        Ok(VmValue::Nil)
    }

    vm.register_builtin("mcp_tools", |args, _out| register_tools_impl(args));
    // Keep old name as alias for backwards compatibility
    vm.register_builtin("mcp_serve", |args, _out| register_tools_impl(args));

    // ---- static resource ----
    // mcp_resource({uri, name, text, description?, mime_type?}) -> nil
    vm.register_builtin("mcp_resource", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource: argument must be a dict with {uri, name, text}".into(),
                ));
            }
        };

        let uri = dict
            .get("uri")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'uri' is required".into()))?;
        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());
        let mime_type = dict.get("mime_type").map(|v| v.display());
        let text = dict
            .get("text")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource: 'text' is required".into()))?;

        MCP_SERVE_RESOURCES.with(|cell| {
            cell.borrow_mut().push(McpResourceDef {
                uri,
                name,
                title,
                description,
                mime_type,
                text,
            });
        });

        Ok(VmValue::Nil)
    });

    // ---- resource template ----
    // mcp_resource_template({uri_template, name, handler, description?, mime_type?}) -> nil
    //
    // The handler receives a dict of URI template arguments and returns a string.
    vm.register_builtin("mcp_resource_template", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: argument must be a dict".into(),
                ));
            }
        };

        let uri_template = dict
            .get("uri_template")
            .map(|v| v.display())
            .ok_or_else(|| {
                VmError::Runtime("mcp_resource_template: 'uri_template' is required".into())
            })?;
        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_resource_template: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());
        let mime_type = dict.get("mime_type").map(|v| v.display());
        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_resource_template: 'handler' closure is required".into(),
                ));
            }
        };

        MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| {
            cell.borrow_mut().push(McpResourceTemplateDef {
                uri_template,
                name,
                title,
                description,
                mime_type,
                handler,
            });
        });

        Ok(VmValue::Nil)
    });

    // ---- prompt ----
    // mcp_prompt({name, handler, description?, arguments?}) -> nil
    vm.register_builtin("mcp_prompt", |args, _out| {
        let dict = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: argument must be a dict with {name, handler}".into(),
                ));
            }
        };

        let name = dict
            .get("name")
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("mcp_prompt: 'name' is required".into()))?;
        let title = dict.get("title").map(|v| v.display());
        let description = dict.get("description").map(|v| v.display());

        let handler = match dict.get("handler") {
            Some(VmValue::Closure(c)) => (**c).clone(),
            _ => {
                return Err(VmError::Runtime(
                    "mcp_prompt: 'handler' closure is required".into(),
                ));
            }
        };

        let arguments = dict.get("arguments").and_then(|v| {
            if let VmValue::List(list) = v {
                let args: Vec<McpPromptArgDef> = list
                    .iter()
                    .filter_map(|item| {
                        if let VmValue::Dict(d) = item {
                            Some(McpPromptArgDef {
                                name: d.get("name").map(|v| v.display()).unwrap_or_default(),
                                description: d.get("description").map(|v| v.display()),
                                required: matches!(d.get("required"), Some(VmValue::Bool(true))),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                if args.is_empty() {
                    None
                } else {
                    Some(args)
                }
            } else {
                None
            }
        });

        MCP_SERVE_PROMPTS.with(|cell| {
            cell.borrow_mut().push(McpPromptDef {
                name,
                title,
                description,
                arguments,
                handler,
            });
        });

        Ok(VmValue::Nil)
    });
}

// =============================================================================
// Thread-local accessors (used by CLI after pipeline execution)
// =============================================================================

pub fn take_mcp_serve_registry() -> Option<VmValue> {
    MCP_SERVE_REGISTRY.with(|cell| cell.borrow_mut().take())
}

pub fn take_mcp_serve_resources() -> Vec<McpResourceDef> {
    MCP_SERVE_RESOURCES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_resource_templates() -> Vec<McpResourceTemplateDef> {
    MCP_SERVE_RESOURCE_TEMPLATES.with(|cell| cell.borrow_mut().drain(..).collect())
}

pub fn take_mcp_serve_prompts() -> Vec<McpPromptDef> {
    MCP_SERVE_PROMPTS.with(|cell| cell.borrow_mut().drain(..).collect())
}

/// MCP protocol version.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Default page size for cursor-based pagination.
const DEFAULT_PAGE_SIZE: usize = 50;

// =============================================================================
// Definitions
// =============================================================================

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

// =============================================================================
// Server
// =============================================================================

/// MCP server that exposes Harn tools, resources, and prompts over stdio JSON-RPC.
pub struct McpServer {
    server_name: String,
    server_version: String,
    tools: Vec<McpToolDef>,
    resources: Vec<McpResourceDef>,
    resource_templates: Vec<McpResourceTemplateDef>,
    prompts: Vec<McpPromptDef>,
    log_level: RefCell<String>,
}

impl McpServer {
    pub fn new(
        server_name: String,
        tools: Vec<McpToolDef>,
        resources: Vec<McpResourceDef>,
        resource_templates: Vec<McpResourceTemplateDef>,
        prompts: Vec<McpPromptDef>,
    ) -> Self {
        Self {
            server_name,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            tools,
            resources,
            resource_templates,
            prompts,
            log_level: RefCell::new("warning".to_string()),
        }
    }

    /// Run the MCP server loop, reading JSON-RPC from stdin and writing to stdout.
    pub async fn run(&self, vm: &mut Vm) -> Result<(), VmError> {
        let stdin = BufReader::new(tokio::io::stdin());
        let mut stdout = tokio::io::stdout();
        let mut lines = stdin.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let id = msg.get("id").cloned();
            let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

            // Notifications (no id) — handle silently
            if id.is_none() {
                continue;
            }
            let id = id.unwrap();

            let response = match method {
                "initialize" => self.handle_initialize(&id),
                "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                "logging/setLevel" => self.handle_logging_set_level(&id, &params),
                "tools/list" => self.handle_tools_list(&id, &params),
                "tools/call" => self.handle_tools_call(&id, &params, vm).await,
                "resources/list" => self.handle_resources_list(&id, &params),
                "resources/read" => self.handle_resources_read(&id, &params, vm).await,
                "resources/templates/list" => {
                    self.handle_resource_templates_list(&id, &params)
                }
                "prompts/list" => self.handle_prompts_list(&id, &params),
                "prompts/get" => self.handle_prompts_get(&id, &params, vm).await,
                _ => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}")
                    }
                }),
            };

            let mut response_line = serde_json::to_string(&response)
                .map_err(|e| VmError::Runtime(format!("MCP server serialization error: {e}")))?;
            response_line.push('\n');
            stdout
                .write_all(response_line.as_bytes())
                .await
                .map_err(|e| VmError::Runtime(format!("MCP server write error: {e}")))?;
            stdout
                .flush()
                .await
                .map_err(|e| VmError::Runtime(format!("MCP server flush error: {e}")))?;
        }

        Ok(())
    }

    fn handle_initialize(&self, id: &serde_json::Value) -> serde_json::Value {
        let mut capabilities = serde_json::Map::new();
        if !self.tools.is_empty() {
            capabilities.insert("tools".into(), serde_json::json!({}));
        }
        if !self.resources.is_empty() || !self.resource_templates.is_empty() {
            capabilities.insert("resources".into(), serde_json::json!({}));
        }
        if !self.prompts.is_empty() {
            capabilities.insert("prompts".into(), serde_json::json!({}));
        }
        capabilities.insert("logging".into(), serde_json::json!({}));

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": capabilities,
                "serverInfo": {
                    "name": self.server_name,
                    "version": self.server_version
                }
            }
        })
    }

    // =========================================================================
    // Tools
    // =========================================================================

    fn handle_tools_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (offset, page_size) = parse_cursor(params);
        let page_end = (offset + page_size).min(self.tools.len());
        let tools: Vec<serde_json::Value> = self.tools[offset..page_end]
            .iter()
            .map(|t| {
                let mut entry = serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                });
                if let Some(ref title) = t.title {
                    entry["title"] = serde_json::json!(title);
                }
                if let Some(ref output_schema) = t.output_schema {
                    entry["outputSchema"] = output_schema.clone();
                }
                if let Some(ref annotations) = t.annotations {
                    entry["annotations"] = annotations.clone();
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({ "tools": tools });
        if page_end < self.tools.len() {
            result["nextCursor"] = serde_json::json!(encode_cursor(page_end));
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
    }

    async fn handle_tools_call(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");

        let tool = match self.tools.iter().find(|t| t.name == tool_name) {
            Some(t) => t,
            None => {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": format!("Unknown tool: {tool_name}") }
                });
            }
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let args_vm = json_to_vm_value(&arguments);

        let result = vm.call_closure_pub(&tool.handler, &[args_vm], &[]).await;

        match result {
            Ok(value) => {
                let content = vm_value_to_content(&value);
                let mut call_result = serde_json::json!({
                    "content": content,
                    "isError": false
                });
                if tool.output_schema.is_some() {
                    let text = value.display();
                    let structured = match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(v) => v,
                        _ => serde_json::json!(text),
                    };
                    call_result["structuredContent"] = structured;
                }
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": call_result
                })
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": format!("{e}") }],
                    "isError": true
                }
            }),
        }
    }

    // =========================================================================
    // Resources
    // =========================================================================

    fn handle_resources_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (offset, page_size) = parse_cursor(params);
        let page_end = (offset + page_size).min(self.resources.len());
        let resources: Vec<serde_json::Value> = self.resources[offset..page_end]
            .iter()
            .map(|r| {
                let mut entry = serde_json::json!({ "uri": r.uri, "name": r.name });
                if let Some(ref title) = r.title {
                    entry["title"] = serde_json::json!(title);
                }
                if let Some(ref desc) = r.description {
                    entry["description"] = serde_json::json!(desc);
                }
                if let Some(ref mime) = r.mime_type {
                    entry["mimeType"] = serde_json::json!(mime);
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({ "resources": resources });
        if page_end < self.resources.len() {
            result["nextCursor"] = serde_json::json!(encode_cursor(page_end));
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
    }

    async fn handle_resources_read(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");

        // Check static resources first
        if let Some(resource) = self.resources.iter().find(|r| r.uri == uri) {
            let mut content = serde_json::json!({ "uri": resource.uri, "text": resource.text });
            if let Some(ref mime) = resource.mime_type {
                content["mimeType"] = serde_json::json!(mime);
            }
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "contents": [content] }
            });
        }

        // Try to match against resource templates
        for tmpl in &self.resource_templates {
            if let Some(args) = match_uri_template(&tmpl.uri_template, uri) {
                let args_vm = json_to_vm_value(&serde_json::json!(args));
                let result = vm.call_closure_pub(&tmpl.handler, &[args_vm], &[]).await;
                return match result {
                    Ok(value) => {
                        let mut content = serde_json::json!({
                            "uri": uri,
                            "text": value.display(),
                        });
                        if let Some(ref mime) = tmpl.mime_type {
                            content["mimeType"] = serde_json::json!(mime);
                        }
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "contents": [content] }
                        })
                    }
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": format!("{e}") }
                    }),
                };
            }
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32002, "message": format!("Resource not found: {uri}") }
        })
    }

    fn handle_resource_templates_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (offset, page_size) = parse_cursor(params);
        let page_end = (offset + page_size).min(self.resource_templates.len());
        let templates: Vec<serde_json::Value> = self.resource_templates[offset..page_end]
            .iter()
            .map(|t| {
                let mut entry =
                    serde_json::json!({ "uriTemplate": t.uri_template, "name": t.name });
                if let Some(ref title) = t.title {
                    entry["title"] = serde_json::json!(title);
                }
                if let Some(ref desc) = t.description {
                    entry["description"] = serde_json::json!(desc);
                }
                if let Some(ref mime) = t.mime_type {
                    entry["mimeType"] = serde_json::json!(mime);
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({ "resourceTemplates": templates });
        if page_end < self.resource_templates.len() {
            result["nextCursor"] = serde_json::json!(encode_cursor(page_end));
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
    }

    // =========================================================================
    // Prompts
    // =========================================================================

    fn handle_prompts_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (offset, page_size) = parse_cursor(params);
        let page_end = (offset + page_size).min(self.prompts.len());
        let prompts: Vec<serde_json::Value> = self.prompts[offset..page_end]
            .iter()
            .map(|p| {
                let mut entry = serde_json::json!({ "name": p.name });
                if let Some(ref title) = p.title {
                    entry["title"] = serde_json::json!(title);
                }
                if let Some(ref desc) = p.description {
                    entry["description"] = serde_json::json!(desc);
                }
                if let Some(ref args) = p.arguments {
                    let args_json: Vec<serde_json::Value> = args
                        .iter()
                        .map(|a| {
                            let mut arg =
                                serde_json::json!({ "name": a.name, "required": a.required });
                            if let Some(ref desc) = a.description {
                                arg["description"] = serde_json::json!(desc);
                            }
                            arg
                        })
                        .collect();
                    entry["arguments"] = serde_json::json!(args_json);
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({ "prompts": prompts });
        if page_end < self.prompts.len() {
            result["nextCursor"] = serde_json::json!(encode_cursor(page_end));
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
    }

    // =========================================================================
    // Logging
    // =========================================================================

    fn handle_logging_set_level(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let level = params
            .get("level")
            .and_then(|l| l.as_str())
            .unwrap_or("warning");
        *self.log_level.borrow_mut() = level.to_string();
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    }

    async fn handle_prompts_get(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");

        let prompt = match self.prompts.iter().find(|p| p.name == name) {
            Some(p) => p,
            None => {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": format!("Unknown prompt: {name}") }
                });
            }
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let args_vm = json_to_vm_value(&arguments);

        let result = vm.call_closure_pub(&prompt.handler, &[args_vm], &[]).await;

        match result {
            Ok(value) => {
                let messages = prompt_value_to_messages(&value);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "messages": messages }
                })
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("{e}") }
            }),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Encode an offset as a base64 cursor string.
fn encode_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

/// Decode a cursor from the request params, returning `(offset, page_size)`.
fn parse_cursor(params: &serde_json::Value) -> (usize, usize) {
    let offset = params
        .get("cursor")
        .and_then(|c| c.as_str())
        .and_then(|c| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(c).ok()?;
            let s = String::from_utf8(bytes).ok()?;
            s.parse::<usize>().ok()
        })
        .unwrap_or(0);
    (offset, DEFAULT_PAGE_SIZE)
}

/// Convert a VmValue returned by a prompt handler into MCP messages.
fn prompt_value_to_messages(value: &VmValue) -> Vec<serde_json::Value> {
    match value {
        VmValue::String(s) => {
            vec![serde_json::json!({
                "role": "user",
                "content": { "type": "text", "text": &**s }
            })]
        }
        VmValue::List(items) => items
            .iter()
            .map(|item| {
                if let VmValue::Dict(d) = item {
                    let role = d
                        .get("role")
                        .map(|v| v.display())
                        .unwrap_or_else(|| "user".into());
                    let content = d.get("content").map(|v| v.display()).unwrap_or_default();
                    serde_json::json!({
                        "role": role,
                        "content": { "type": "text", "text": content }
                    })
                } else {
                    serde_json::json!({
                        "role": "user",
                        "content": { "type": "text", "text": item.display() }
                    })
                }
            })
            .collect(),
        _ => {
            vec![serde_json::json!({
                "role": "user",
                "content": { "type": "text", "text": value.display() }
            })]
        }
    }
}

/// Simple URI template matching (RFC 6570 Level 1 only).
///
/// Matches a URI against a template like `file:///{path}` and extracts named
/// variables. Returns `None` if the URI doesn't match the template structure.
fn match_uri_template(
    template: &str,
    uri: &str,
) -> Option<std::collections::HashMap<String, String>> {
    let mut vars = std::collections::HashMap::new();
    let mut t_pos = 0;
    let mut u_pos = 0;
    let t_bytes = template.as_bytes();
    let u_bytes = uri.as_bytes();

    while t_pos < t_bytes.len() {
        if t_bytes[t_pos] == b'{' {
            // Find the closing brace
            let close = template[t_pos..].find('}')? + t_pos;
            let var_name = &template[t_pos + 1..close];
            t_pos = close + 1;

            // Capture everything up to the next literal in the template (or end)
            let next_literal = if t_pos < t_bytes.len() {
                // Find how much literal follows
                let lit_start = t_pos;
                let lit_end = template[t_pos..]
                    .find('{')
                    .map(|i| t_pos + i)
                    .unwrap_or(t_bytes.len());
                Some(&template[lit_start..lit_end])
            } else {
                None
            };

            let value_end = match next_literal {
                Some(lit) if !lit.is_empty() => uri[u_pos..].find(lit).map(|i| u_pos + i)?,
                _ => u_bytes.len(),
            };

            vars.insert(var_name.to_string(), uri[u_pos..value_end].to_string());
            u_pos = value_end;
        } else {
            // Literal character must match
            if u_pos >= u_bytes.len() || t_bytes[t_pos] != u_bytes[u_pos] {
                return None;
            }
            t_pos += 1;
            u_pos += 1;
        }
    }

    if u_pos == u_bytes.len() {
        Some(vars)
    } else {
        None
    }
}

/// Convert a tool result VmValue into MCP content items.
///
/// Supports text, embedded resource, and resource_link content types.
/// If the value is a list of dicts with a `type` field, each is treated as a
/// content item. Otherwise, the whole value is serialized as a single text item.
fn vm_value_to_content(value: &VmValue) -> Vec<serde_json::Value> {
    if let VmValue::List(items) = value {
        let mut content = Vec::new();
        for item in items.iter() {
            if let VmValue::Dict(d) = item {
                let item_type = d.get("type").map(|v| v.display()).unwrap_or_default();
                match item_type.as_str() {
                    "resource" => {
                        let mut entry = serde_json::json!({ "type": "resource" });
                        if let Some(resource) = d.get("resource") {
                            entry["resource"] = vm_value_to_json(resource);
                        }
                        content.push(entry);
                    }
                    "resource_link" => {
                        let mut entry = serde_json::json!({ "type": "resource_link" });
                        if let Some(uri) = d.get("uri") {
                            entry["uri"] = serde_json::json!(uri.display());
                        }
                        if let Some(name) = d.get("name") {
                            entry["name"] = serde_json::json!(name.display());
                        }
                        if let Some(desc) = d.get("description") {
                            entry["description"] = serde_json::json!(desc.display());
                        }
                        if let Some(mime) = d.get("mimeType") {
                            entry["mimeType"] = serde_json::json!(mime.display());
                        }
                        content.push(entry);
                    }
                    _ => {
                        let text = d
                            .get("text")
                            .map(|v| v.display())
                            .unwrap_or_else(|| item.display());
                        content.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                }
            } else {
                content.push(serde_json::json!({ "type": "text", "text": item.display() }));
            }
        }
        if content.is_empty() {
            vec![serde_json::json!({ "type": "text", "text": value.display() })]
        } else {
            content
        }
    } else {
        vec![serde_json::json!({ "type": "text", "text": value.display() })]
    }
}

/// Convert a VmValue to a serde_json::Value.
fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Int(n) => serde_json::json!(n),
        VmValue::Float(f) => serde_json::json!(f),
        VmValue::String(s) => serde_json::json!(&**s),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => {
            let mut map = serde_json::Map::new();
            for (k, v) in d.iter() {
                map.insert(k.clone(), vm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        _ => serde_json::json!(value.display()),
    }
}

/// Convert a VmValue annotations dict to a serde_json::Value with only the
/// recognized MCP annotation fields.
fn annotations_to_json(annotations: &VmValue) -> Option<serde_json::Value> {
    let dict = match annotations {
        VmValue::Dict(d) => d,
        _ => return None,
    };

    let mut out = serde_json::Map::new();
    let str_keys = ["title"];
    let bool_keys = [
        "readOnlyHint",
        "destructiveHint",
        "idempotentHint",
        "openWorldHint",
    ];

    for key in str_keys {
        if let Some(VmValue::String(s)) = dict.get(key) {
            out.insert(key.into(), serde_json::json!(&**s));
        }
    }
    for key in bool_keys {
        if let Some(VmValue::Bool(b)) = dict.get(key) {
            out.insert(key.into(), serde_json::json!(b));
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(out))
    }
}

// =============================================================================
// Tool registry extraction
// =============================================================================

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<Vec<McpToolDef>, VmError> {
    let dict = match registry {
        VmValue::Dict(d) => d,
        _ => {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry".into(),
            ));
        }
    };

    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => {}
        _ => {
            return Err(VmError::Runtime(
                "mcp_tools: argument must be a tool registry (created with tool_registry())".into(),
            ));
        }
    }

    let tools = match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => return Ok(Vec::new()),
    };

    let mut mcp_tools = Vec::new();
    for tool in tools.iter() {
        if let VmValue::Dict(entry) = tool {
            let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
            let title = entry.get("title").map(|v| v.display());
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();

            let handler = match entry.get("handler") {
                Some(VmValue::Closure(c)) => (**c).clone(),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "mcp_tools: tool '{name}' has no handler closure"
                    )));
                }
            };

            let input_schema = params_to_json_schema(entry.get("parameters"));
            let output_schema = entry.get("output_schema").and_then(|v| {
                if let VmValue::Dict(_) = v {
                    Some(vm_value_to_json(v))
                } else {
                    None
                }
            });
            let annotations = entry.get("annotations").and_then(annotations_to_json);

            mcp_tools.push(McpToolDef {
                name,
                title,
                description,
                input_schema,
                output_schema,
                annotations,
                handler,
            });
        }
    }

    Ok(mcp_tools)
}

/// Convert Harn tool_define parameter definitions to JSON Schema for MCP inputSchema.
fn params_to_json_schema(params: Option<&VmValue>) -> serde_json::Value {
    let params_dict = match params {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return serde_json::json!({ "type": "object", "properties": {} });
        }
    };

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for (param_name, param_def) in params_dict.iter() {
        if let VmValue::Dict(def) = param_def {
            let mut prop = serde_json::Map::new();
            if let Some(VmValue::String(t)) = def.get("type") {
                prop.insert("type".into(), serde_json::Value::String(t.to_string()));
            }
            if let Some(VmValue::String(d)) = def.get("description") {
                prop.insert(
                    "description".into(),
                    serde_json::Value::String(d.to_string()),
                );
            }
            if matches!(def.get("required"), Some(VmValue::Bool(true))) {
                required.push(serde_json::Value::String(param_name.clone()));
            }
            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        } else if let VmValue::String(type_str) = param_def {
            let mut prop = serde_json::Map::new();
            prop.insert(
                "type".into(),
                serde_json::Value::String(type_str.to_string()),
            );
            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        }
    }

    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), serde_json::Value::String("object".into()));
    schema.insert("properties".into(), serde_json::Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), serde_json::Value::Array(required));
    }
    serde_json::Value::Object(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[test]
    fn test_params_to_json_schema_empty() {
        let schema = params_to_json_schema(None);
        assert_eq!(
            schema,
            serde_json::json!({ "type": "object", "properties": {} })
        );
    }

    #[test]
    fn test_params_to_json_schema_with_params() {
        let mut params = BTreeMap::new();
        let mut param_def = BTreeMap::new();
        param_def.insert("type".to_string(), VmValue::String(Rc::from("string")));
        param_def.insert(
            "description".to_string(),
            VmValue::String(Rc::from("A file path")),
        );
        param_def.insert("required".to_string(), VmValue::Bool(true));
        params.insert("path".to_string(), VmValue::Dict(Rc::new(param_def)));

        let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
        assert_eq!(
            schema,
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "A file path" } },
                "required": ["path"]
            })
        );
    }

    #[test]
    fn test_params_to_json_schema_simple_form() {
        let mut params = BTreeMap::new();
        params.insert("query".to_string(), VmValue::String(Rc::from("string")));
        let schema = params_to_json_schema(Some(&VmValue::Dict(Rc::new(params))));
        assert_eq!(
            schema["properties"]["query"]["type"],
            serde_json::json!("string")
        );
    }

    #[test]
    fn test_tool_registry_to_mcp_tools_invalid() {
        assert!(tool_registry_to_mcp_tools(&VmValue::Nil).is_err());
    }

    #[test]
    fn test_tool_registry_to_mcp_tools_empty() {
        let mut registry = BTreeMap::new();
        registry.insert("_type".into(), VmValue::String(Rc::from("tool_registry")));
        registry.insert("tools".into(), VmValue::List(Rc::new(Vec::new())));
        let result = tool_registry_to_mcp_tools(&VmValue::Dict(Rc::new(registry)));
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_prompt_value_to_messages_string() {
        let msgs = prompt_value_to_messages(&VmValue::String(Rc::from("hello")));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"]["text"], "hello");
    }

    #[test]
    fn test_prompt_value_to_messages_list() {
        let items = vec![
            VmValue::Dict(Rc::new({
                let mut d = BTreeMap::new();
                d.insert("role".into(), VmValue::String(Rc::from("user")));
                d.insert("content".into(), VmValue::String(Rc::from("hi")));
                d
            })),
            VmValue::Dict(Rc::new({
                let mut d = BTreeMap::new();
                d.insert("role".into(), VmValue::String(Rc::from("assistant")));
                d.insert("content".into(), VmValue::String(Rc::from("hello")));
                d
            })),
        ];
        let msgs = prompt_value_to_messages(&VmValue::List(Rc::new(items)));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn test_match_uri_template_simple() {
        let vars = match_uri_template("file:///{path}", "file:///foo/bar.rs").unwrap();
        assert_eq!(vars["path"], "foo/bar.rs");
    }

    #[test]
    fn test_match_uri_template_multiple() {
        let vars = match_uri_template("db://{schema}/{table}", "db://public/users").unwrap();
        assert_eq!(vars["schema"], "public");
        assert_eq!(vars["table"], "users");
    }

    #[test]
    fn test_match_uri_template_no_match() {
        assert!(match_uri_template("file:///{path}", "http://example.com").is_none());
    }

    #[test]
    fn test_annotations_to_json() {
        let mut d = BTreeMap::new();
        d.insert("title".into(), VmValue::String(Rc::from("My Tool")));
        d.insert("readOnlyHint".into(), VmValue::Bool(true));
        d.insert("destructiveHint".into(), VmValue::Bool(false));
        let json = annotations_to_json(&VmValue::Dict(Rc::new(d))).unwrap();
        assert_eq!(json["title"], "My Tool");
        assert_eq!(json["readOnlyHint"], true);
        assert_eq!(json["destructiveHint"], false);
    }

    #[test]
    fn test_annotations_empty_returns_none() {
        let d = BTreeMap::new();
        assert!(annotations_to_json(&VmValue::Dict(Rc::new(d))).is_none());
    }
}
