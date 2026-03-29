//! MCP server mode: expose Harn tool registries as MCP tools over stdio.
//!
//! This is the mirror of `mcp.rs` (the client). A Harn pipeline defines tools
//! via `tool_registry()` + `tool_define()`, then the CLI's `mcp-serve` command
//! starts this server, making those tools callable by Claude Desktop, Cursor,
//! or any MCP client.

use std::cell::RefCell;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::stdlib::json_to_vm_value;
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    /// Stores the tool registry set by the `mcp_serve` builtin.
    /// The CLI reads this after pipeline execution to start the MCP server loop.
    static MCP_SERVE_REGISTRY: RefCell<Option<VmValue>> = const { RefCell::new(None) };
}

/// Register the `mcp_serve` builtin on a VM.
pub fn register_mcp_server_builtins(vm: &mut Vm) {
    vm.register_builtin("mcp_serve", |args, _out| {
        let registry = args.first().cloned().ok_or_else(|| {
            VmError::Runtime("mcp_serve: requires a tool_registry argument".into())
        })?;

        // Validate it's a tool_registry
        if let VmValue::Dict(d) = &registry {
            match d.get("_type") {
                Some(VmValue::String(t)) if &**t == "tool_registry" => {}
                _ => {
                    return Err(VmError::Runtime(
                        "mcp_serve: argument must be a tool registry (created with tool_registry())"
                            .into(),
                    ));
                }
            }
        } else {
            return Err(VmError::Runtime(
                "mcp_serve: argument must be a tool registry".into(),
            ));
        }

        MCP_SERVE_REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(registry);
        });

        Ok(VmValue::Nil)
    });
}

/// Take the tool registry that was set by a `mcp_serve()` call.
/// Returns `None` if `mcp_serve` was never called.
pub fn take_mcp_serve_registry() -> Option<VmValue> {
    MCP_SERVE_REGISTRY.with(|cell| cell.borrow_mut().take())
}

/// MCP protocol version.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A tool extracted from a Harn tool_registry, ready to serve over MCP.
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub handler: VmClosure,
}

/// MCP server that exposes Harn tools over stdio JSON-RPC.
pub struct McpServer {
    server_name: String,
    server_version: String,
    tools: Vec<McpToolDef>,
}

impl McpServer {
    pub fn new(server_name: String, tools: Vec<McpToolDef>) -> Self {
        Self {
            server_name,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            tools,
        }
    }

    /// Run the MCP server loop, reading JSON-RPC from stdin and writing to stdout.
    /// This blocks forever (until stdin closes or the process is killed).
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
                // notifications/initialized, notifications/cancelled, etc.
                continue;
            }

            let id = id.unwrap();

            let response = match method {
                "initialize" => self.handle_initialize(&id),
                "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                "tools/list" => self.handle_tools_list(&id),
                "tools/call" => self.handle_tools_call(&id, &params, vm).await,
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
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": self.server_name,
                    "version": self.server_version
                }
            }
        })
    }

    fn handle_tools_list(&self, id: &serde_json::Value) -> serde_json::Value {
        let tools: Vec<serde_json::Value> = self
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect();

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools }
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
                    "error": {
                        "code": -32602,
                        "message": format!("Unknown tool: {tool_name}")
                    }
                });
            }
        };

        // Convert MCP arguments to a VmValue::Dict
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let args_vm = json_to_vm_value(&arguments);

        // Invoke the handler closure with the arguments dict
        let result = vm.call_closure_pub(&tool.handler, &[args_vm], &[]).await;

        match result {
            Ok(value) => {
                let text = value.display();
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    }
                })
            }
            Err(e) => {
                let error_text = format!("{e}");
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": error_text }],
                        "isError": true
                    }
                })
            }
        }
    }
}

/// Extract tools from a Harn tool_registry VmValue and convert to MCP tool definitions.
pub fn tool_registry_to_mcp_tools(registry: &VmValue) -> Result<Vec<McpToolDef>, VmError> {
    let dict = match registry {
        VmValue::Dict(d) => d,
        _ => {
            return Err(VmError::Runtime(
                "mcp_serve: argument must be a tool registry".into(),
            ));
        }
    };

    // Validate it's a tool_registry
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => {}
        _ => {
            return Err(VmError::Runtime(
                "mcp_serve: argument must be a tool registry (created with tool_registry())".into(),
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
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();

            let handler = match entry.get("handler") {
                Some(VmValue::Closure(c)) => (**c).clone(),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "mcp_serve: tool '{name}' has no handler closure"
                    )));
                }
            };

            let input_schema = params_to_json_schema(entry.get("parameters"));

            mcp_tools.push(McpToolDef {
                name,
                description,
                input_schema,
                handler,
            });
        }
    }

    Ok(mcp_tools)
}

/// Convert Harn tool_define parameter definitions to JSON Schema for MCP inputSchema.
///
/// Input format (from tool_define):
/// ```text
/// { param_name: { type: "string", description: "...", required: true } }
/// ```
///
/// Output format (JSON Schema):
/// ```json
/// { "type": "object", "properties": { "param_name": { "type": "string", "description": "..." } }, "required": ["param_name"] }
/// ```
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
                prop.insert("type".to_string(), serde_json::Value::String(t.to_string()));
            }
            if let Some(VmValue::String(d)) = def.get("description") {
                prop.insert(
                    "description".to_string(),
                    serde_json::Value::String(d.to_string()),
                );
            }

            // Check if required (defaults to false)
            let is_required = matches!(def.get("required"), Some(VmValue::Bool(true)));
            if is_required {
                required.push(serde_json::Value::String(param_name.clone()));
            }

            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        } else if let VmValue::String(type_str) = param_def {
            // Simple form: { param_name: "string" }
            let mut prop = serde_json::Map::new();
            prop.insert(
                "type".to_string(),
                serde_json::Value::String(type_str.to_string()),
            );
            properties.insert(param_name.clone(), serde_json::Value::Object(prop));
        }
    }

    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    schema.insert(
        "properties".to_string(),
        serde_json::Value::Object(properties),
    );
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::Value::Array(required));
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
        let expected = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "A file path"
                }
            },
            "required": ["path"]
        });
        assert_eq!(schema, expected);
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
        let result = tool_registry_to_mcp_tools(&VmValue::Nil);
        assert!(result.is_err());
    }

    #[test]
    fn test_tool_registry_to_mcp_tools_empty() {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from("tool_registry")),
        );
        registry.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));

        let result = tool_registry_to_mcp_tools(&VmValue::Dict(Rc::new(registry)));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
