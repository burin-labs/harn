//! MCP (Model Context Protocol) client for connecting to external tool servers.
//!
//! Supports stdio transport: spawns a child process and communicates via
//! newline-delimited JSON-RPC 2.0 over stdin/stdout.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// MCP protocol version (2024-11-05 is widely supported).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Default timeout for MCP requests (60 seconds).
const MCP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Internal state for an MCP client connection.
struct McpClientInner {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

/// Handle to an MCP client connection, stored in VmValue.
///
/// Cloning the handle shares access to the same connection (via Arc).
#[derive(Clone)]
pub struct VmMcpClientHandle {
    pub name: String,
    inner: Arc<Mutex<Option<McpClientInner>>>,
}

impl std::fmt::Debug for VmMcpClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "McpClient({})", self.name)
    }
}

impl VmMcpClientHandle {
    /// Send a JSON-RPC request and wait for the response.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        let id = inner.next_id;
        inner.next_id += 1;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let line = serde_json::to_string(&request)
            .map_err(|e| VmError::Runtime(format!("MCP serialization error: {e}")))?;
        inner
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
        inner
            .stdin
            .write_all(b"\n")
            .await
            .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
        inner
            .stdin
            .flush()
            .await
            .map_err(|e| VmError::Runtime(format!("MCP flush error: {e}")))?;

        // Read lines until we get a response with matching ID
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let bytes_read =
                tokio::time::timeout(MCP_TIMEOUT, inner.reader.read_line(&mut line_buf))
                    .await
                    .map_err(|_| {
                        VmError::Runtime(format!(
                            "MCP: server did not respond to '{method}' within {}s",
                            MCP_TIMEOUT.as_secs()
                        ))
                    })?
                    .map_err(|e| VmError::Runtime(format!("MCP read error: {e}")))?;

            if bytes_read == 0 {
                return Err(VmError::Runtime("MCP: server closed connection".into()));
            }

            let trimmed = line_buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Skip notifications (no id)
            if msg.get("id").is_none() {
                continue;
            }

            // Check if this is our response
            if msg["id"].as_u64() == Some(id) {
                if let Some(error) = msg.get("error") {
                    let message = error["message"].as_str().unwrap_or("Unknown MCP error");
                    let code = error["code"].as_i64().unwrap_or(-1);
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "MCP error ({code}): {message}"
                    )))));
                }
                return Ok(msg["result"].clone());
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), VmError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| VmError::Runtime("MCP client is disconnected".into()))?;

        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let line = serde_json::to_string(&notification)
            .map_err(|e| VmError::Runtime(format!("MCP serialization error: {e}")))?;
        inner
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
        inner
            .stdin
            .write_all(b"\n")
            .await
            .map_err(|e| VmError::Runtime(format!("MCP write error: {e}")))?;
        inner
            .stdin
            .flush()
            .await
            .map_err(|e| VmError::Runtime(format!("MCP flush error: {e}")))?;
        Ok(())
    }
}

/// Connect to an MCP server by spawning a child process (stdio transport).
async fn mcp_connect_impl(command: &str, args: &[String]) -> Result<VmMcpClientHandle, VmError> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "mcp_connect: failed to spawn '{command}': {e}"
            ))))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VmError::Runtime("mcp_connect: failed to open stdout".into()))?;

    let reader = BufReader::new(stdout);
    let name = command.to_string();
    let inner = McpClientInner {
        child,
        stdin,
        reader,
        next_id: 1,
    };

    let handle = VmMcpClientHandle {
        name: name.clone(),
        inner: Arc::new(Mutex::new(Some(inner))),
    };

    // Initialize handshake
    handle
        .call(
            "initialize",
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await?;

    // Send initialized notification
    handle
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;

    Ok(handle)
}

/// Convert a VmValue to serde_json::Value for MCP tool call arguments.
pub(crate) fn vm_value_to_serde(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_serde).collect())
        }
        VmValue::Dict(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

/// Extract text content from an MCP tool result.
///
/// MCP returns `{"content": [{"type": "text", "text": "..."}], "isError": false}`.
/// This extracts and concatenates all text content blocks.
fn extract_content_text(result: &serde_json::Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if texts.is_empty() {
            // No text blocks — return full result as JSON for inspection
            json_to_vm_value(result).display()
        } else {
            texts.join("\n")
        }
    } else {
        json_to_vm_value(result).display()
    }
}

/// Connect to an MCP server by name, command, and args. This is the public
/// entry point used by the CLI to auto-connect servers declared in `harn.toml`.
pub async fn connect_mcp_server(
    name: &str,
    command: &str,
    args: &[String],
) -> Result<VmMcpClientHandle, VmError> {
    let mut handle = mcp_connect_impl(command, args).await?;
    // Override the name with the user-declared name from config
    handle.name = name.to_string();
    Ok(handle)
}

/// Register MCP builtins on a VM.
pub fn register_mcp_builtins(vm: &mut Vm) {
    // mcp_connect(command, args?) -> McpClient
    vm.register_async_builtin("mcp_connect", |args| async move {
        let command = args.first().map(|a| a.display()).unwrap_or_default();
        if command.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_connect: command is required",
            ))));
        }

        let cmd_args: Vec<String> = match args.get(1) {
            Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
            _ => Vec::new(),
        };

        let handle = mcp_connect_impl(&command, &cmd_args).await?;
        Ok(VmValue::McpClient(handle))
    });

    // mcp_list_tools(client) -> List of tool dicts
    vm.register_async_builtin("mcp_list_tools", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_tools: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("tools/list", serde_json::json!({})).await?;

        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_tools: Vec<VmValue> = tools.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_tools)))
    });

    // mcp_call(client, tool_name, arguments?) -> String result
    vm.register_async_builtin("mcp_call", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_call: first argument must be an MCP client",
                ))));
            }
        };

        let tool_name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if tool_name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_call: tool name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        let result = client
            .call(
                "tools/call",
                serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await?;

        // Check if the tool reported an error
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            let error_text = extract_content_text(&result);
            return Err(VmError::Thrown(VmValue::String(Rc::from(error_text))));
        }

        // Return content as a string for simple results, or a dict for complex ones
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        // If single text block, return as string
        if content.len() == 1 && content[0].get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = content[0].get("text").and_then(|t| t.as_str()) {
                return Ok(VmValue::String(Rc::from(text)));
            }
        }

        // Multiple or non-text blocks: return full content list
        if content.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(VmValue::List(Rc::new(
                content.iter().map(json_to_vm_value).collect(),
            )))
        }
    });

    // mcp_server_info(client) -> Dict with server capabilities
    vm.register_async_builtin("mcp_server_info", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_server_info: argument must be an MCP client",
                ))));
            }
        };

        // Re-issue initialize to get server info (it's idempotent per spec)
        // Actually, we can just list tools as a health check
        let guard = client.inner.lock().await;
        if guard.is_none() {
            return Err(VmError::Runtime("MCP client is disconnected".into()));
        }
        drop(guard);

        let mut info = BTreeMap::new();
        info.insert(
            "name".to_string(),
            VmValue::String(Rc::from(client.name.as_str())),
        );
        info.insert("connected".to_string(), VmValue::Bool(true));
        Ok(VmValue::Dict(Rc::new(info)))
    });

    // mcp_disconnect(client) -> nil
    vm.register_async_builtin("mcp_disconnect", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_disconnect: argument must be an MCP client",
                ))));
            }
        };

        let mut guard = client.inner.lock().await;
        if let Some(mut inner) = guard.take() {
            let _ = inner.child.kill().await;
        }
        Ok(VmValue::Nil)
    });

    // mcp_list_resources(client) -> list of resource dicts
    vm.register_async_builtin("mcp_list_resources", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_resources: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("resources/list", serde_json::json!({})).await?;

        let resources = result
            .get("resources")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_resources: Vec<VmValue> = resources.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_resources)))
    });

    // mcp_read_resource(client, uri) -> string | list
    vm.register_async_builtin("mcp_read_resource", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_read_resource: first argument must be an MCP client",
                ))));
            }
        };

        let uri = args.get(1).map(|a| a.display()).unwrap_or_default();
        if uri.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_read_resource: URI is required",
            ))));
        }

        let result = client
            .call("resources/read", serde_json::json!({ "uri": uri }))
            .await?;

        // Extract content blocks
        let contents = result
            .get("contents")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        // Single text block → return as string
        if contents.len() == 1 {
            if let Some(text) = contents[0].get("text").and_then(|t| t.as_str()) {
                return Ok(VmValue::String(Rc::from(text)));
            }
        }

        if contents.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(VmValue::List(Rc::new(
                contents.iter().map(json_to_vm_value).collect(),
            )))
        }
    });

    // mcp_list_prompts(client) -> list of prompt dicts
    vm.register_async_builtin("mcp_list_prompts", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_list_prompts: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("prompts/list", serde_json::json!({})).await?;

        let prompts = result
            .get("prompts")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_prompts: Vec<VmValue> = prompts.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_prompts)))
    });

    // mcp_get_prompt(client, name, arguments?) -> dict
    vm.register_async_builtin("mcp_get_prompt", |args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "mcp_get_prompt: first argument must be an MCP client",
                ))));
            }
        };

        let name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mcp_get_prompt: prompt name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        let result = client
            .call(
                "prompts/get",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        Ok(json_to_vm_value(&result))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_value_to_serde_string() {
        let val = VmValue::String(Rc::from("hello"));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!("hello"));
    }

    #[test]
    fn test_vm_value_to_serde_dict() {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), VmValue::Int(42));
        let val = VmValue::Dict(Rc::new(map));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!({"key": 42}));
    }

    #[test]
    fn test_vm_value_to_serde_list() {
        let val = VmValue::List(Rc::new(vec![VmValue::Int(1), VmValue::Int(2)]));
        let json = vm_value_to_serde(&val);
        assert_eq!(json, serde_json::json!([1, 2]));
    }

    #[test]
    fn test_extract_content_text_single() {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": "hello world"}],
            "isError": false
        });
        assert_eq!(extract_content_text(&result), "hello world");
    }

    #[test]
    fn test_extract_content_text_multiple() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "line 1"},
                {"type": "text", "text": "line 2"}
            ],
            "isError": false
        });
        assert_eq!(extract_content_text(&result), "line 1\nline 2");
    }

    #[test]
    fn test_extract_content_text_empty() {
        let result = serde_json::json!({"content": [], "isError": false});
        let text = extract_content_text(&result);
        assert!(!text.is_empty()); // falls back to displaying the value
    }

    #[test]
    fn test_mcp_client_handle_debug() {
        let handle = VmMcpClientHandle {
            name: "test-server".to_string(),
            inner: Arc::new(Mutex::new(None)),
        };
        assert_eq!(format!("{:?}", handle), "McpClient(test-server)");
    }
}
