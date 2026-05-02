use std::cell::RefCell;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::mcp_elicit::{install_bus, ElicitationBus};
use crate::stdlib::json_to_vm_value;
use crate::value::VmError;
use crate::vm::Vm;

use super::convert::{prompt_value_to_messages, vm_value_to_content};
use super::defs::{McpPromptDef, McpResourceDef, McpResourceTemplateDef, McpToolDef};
use super::pagination::{encode_cursor, parse_cursor};
use super::uri::match_uri_template;
use super::PROTOCOL_VERSION;

/// MCP server that exposes Harn tools, resources, and prompts over MCP JSON-RPC.
pub struct McpServer {
    server_name: String,
    server_version: String,
    tools: Vec<McpToolDef>,
    resources: Vec<McpResourceDef>,
    resource_templates: Vec<McpResourceTemplateDef>,
    prompts: Vec<McpPromptDef>,
    log_level: RefCell<String>,
    /// Optional Server Card payload — advertised in the `initialize`
    /// response's `serverInfo.card` field and exposed as a static
    /// resource at the well-known URI `well-known://mcp-card`.
    /// Populated by `harn serve mcp --card path/to/card.json`.
    server_card: Option<serde_json::Value>,
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
            server_card: None,
        }
    }

    /// Attach a Server Card to be advertised over `initialize` and via
    /// the `well-known://mcp-card` resource. Call on a freshly-built
    /// `McpServer` before `run`.
    pub fn with_server_card(mut self, card: serde_json::Value) -> Self {
        self.server_card = Some(card);
        self
    }

    /// Run the MCP server loop, reading JSON-RPC from stdin and writing
    /// to stdout. The transport is split across three concurrent halves:
    /// a stdin reader that demuxes responses to in-flight elicitation
    /// requests away from new client requests, a stdout writer task
    /// that drains the outbound queue, and the main dispatch loop that
    /// owns the VM and processes new requests one at a time. This shape
    /// is what allows a tool handler to call `mcp_elicit(...)` mid-flight
    /// — the handler awaits an inbound response while the writer keeps
    /// emitting the elicitation request.
    pub async fn run(&self, vm: &mut Vm) -> Result<(), VmError> {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let bus = ElicitationBus::new(out_tx.clone());

        let bus_for_reader = bus.clone();
        let in_tx_reader = in_tx.clone();
        let reader = tokio::spawn(async move {
            let stdin = BufReader::new(tokio::io::stdin());
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
                // Per JSON-RPC, responses (no `method`, has `id` + result/error)
                // must not themselves be replied to. Route to the
                // elicitation bus when we recognize the id; otherwise
                // silently drop instead of letting the dispatcher
                // bounce a "Method not found" back to the client.
                if msg.get("method").is_none() {
                    let _ = bus_for_reader.route_response(&msg);
                    continue;
                }
                if in_tx_reader.send(msg).is_err() {
                    break;
                }
            }
        });
        // Drop the inbound sender held by the spawn closure's parent
        // scope so closing of stdin actually terminates the dispatcher.
        drop(in_tx);

        let writer = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            while let Some(msg) = out_rx.recv().await {
                let mut line = match serde_json::to_string(&msg) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                line.push('\n');
                if stdout.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.flush().await.is_err() {
                    break;
                }
            }
        });

        // Make the bus visible to tool handlers running on this thread.
        let previous_bus = install_bus(Some(bus));

        while let Some(msg) = in_rx.recv().await {
            if let Some(response) = self.handle_json_rpc(msg, vm).await {
                if out_tx.send(response).is_err() {
                    break;
                }
            }
        }

        // Closing out_tx tells the writer task to drain and exit.
        // Restoring the previous bus (rather than always wiping)
        // keeps nested usages well-defined if they ever appear.
        drop(out_tx);
        install_bus(previous_bus);

        // Best-effort wait so the writer flushes any tail responses
        // before the function returns. Reader task exits naturally
        // when stdin closes.
        let _ = writer.await;
        reader.abort();
        Ok(())
    }

    /// Handle one MCP JSON-RPC message. Notifications return `None`.
    pub async fn handle_json_rpc(
        &self,
        msg: serde_json::Value,
        vm: &mut Vm,
    ) -> Option<serde_json::Value> {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned()?;
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        Some(match method {
            "initialize" => self.handle_initialize(&id),
            "ping" => crate::jsonrpc::response(id.clone(), serde_json::json!({})),
            "logging/setLevel" => self.handle_logging_set_level(&id, &params),
            "harn.hitl.respond" => self.handle_hitl_respond(&id, &params).await,
            "tools/list" => self.handle_tools_list(&id, &params),
            "tools/call" => self.handle_tools_call(&id, &params, vm).await,
            crate::mcp_protocol::METHOD_TASKS_GET => self.handle_task_lookup(&id, &params),
            crate::mcp_protocol::METHOD_TASKS_RESULT => self.handle_task_lookup(&id, &params),
            crate::mcp_protocol::METHOD_TASKS_LIST => self.handle_tasks_list(&id, &params),
            crate::mcp_protocol::METHOD_TASKS_CANCEL => self.handle_task_lookup(&id, &params),
            "resources/list" => self.handle_resources_list(&id, &params),
            "resources/read" => self.handle_resources_read(&id, &params, vm).await,
            "resources/templates/list" => self.handle_resource_templates_list(&id, &params),
            "prompts/list" => self.handle_prompts_list(&id, &params),
            "prompts/get" => self.handle_prompts_get(&id, &params, vm).await,
            _ if crate::mcp_protocol::unsupported_latest_spec_method(method).is_some() => {
                crate::mcp_protocol::unsupported_latest_spec_method_response(id.clone(), method)
                    .expect("checked unsupported MCP method")
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        })
    }

    fn handle_initialize(&self, id: &serde_json::Value) -> serde_json::Value {
        let mut capabilities = serde_json::Map::new();
        if !self.tools.is_empty() {
            capabilities.insert("tools".into(), serde_json::json!({ "listChanged": true }));
        }
        if !self.resources.is_empty()
            || !self.resource_templates.is_empty()
            || self.server_card.is_some()
        {
            capabilities.insert(
                "resources".into(),
                serde_json::json!({ "listChanged": true }),
            );
        }
        if !self.prompts.is_empty() {
            capabilities.insert("prompts".into(), serde_json::json!({ "listChanged": true }));
        }
        capabilities.insert("logging".into(), serde_json::json!({}));
        capabilities.insert("tasks".into(), crate::mcp_protocol::tasks_capability());
        // Always advertise elicitation: any registered tool may decide
        // at runtime whether to call `mcp_elicit(...)`, so capability
        // negotiation can't be tied to a static check.
        capabilities.insert("elicitation".into(), serde_json::json!({}));

        let mut server_info = serde_json::json!({
            "name": self.server_name,
            "version": self.server_version
        });
        if let Some(ref card) = self.server_card {
            server_info["card"] = card.clone();
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": capabilities,
                "serverInfo": server_info
            }
        })
    }

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
                entry["execution"] = crate::mcp_protocol::tool_execution(
                    crate::mcp_protocol::McpToolTaskSupport::Forbidden,
                );
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
        if crate::mcp_protocol::requests_task_augmentation(params) {
            return crate::mcp_protocol::task_augmentation_error_response(
                id.clone(),
                "tools/call",
                -32602,
                "Tool does not support MCP task-augmented execution",
                "This Harn MCP server executes registered Harn closures inline, so every tool advertises execution.taskSupport=\"forbidden\".",
            );
        }

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

        let result = vm.call_closure_pub(&tool.handler, &[args_vm]).await;

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

    fn handle_task_lookup(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let task_id = params
            .get("taskId")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32602,
                "message": format!("Failed to retrieve task: task not found '{task_id}'")
            }
        })
    }

    fn handle_tasks_list(
        &self,
        id: &serde_json::Value,
        _params: &serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tasks": [] }
        })
    }

    async fn handle_hitl_respond(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let response: crate::stdlib::hitl::HitlHostResponse =
            match serde_json::from_value(params.clone()) {
                Ok(response) => response,
                Err(error) => {
                    return serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32602,
                            "message": format!("invalid harn.hitl.respond params: {error}"),
                        }
                    });
                }
            };
        let cwd = std::env::current_dir().ok();
        match crate::stdlib::hitl::append_hitl_response(cwd.as_deref(), response).await {
            Ok(_) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "ok": true }
            }),
            Err(error) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32000,
                    "message": error
                }
            }),
        }
    }

    fn handle_resources_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        // Virtually prepend the Server Card as a static resource so
        // clients that browse resources can discover the card without
        // a separate well-known GET. Kept out of the underlying
        // `self.resources` vec so cursor paging stays simple.
        let card_entry = self.server_card.as_ref().map(|_| {
            serde_json::json!({
                "uri": "well-known://mcp-card",
                "name": "Server Card",
                "description": "MCP v2.1 Server Card advertising this server's identity and capabilities",
                "mimeType": "application/json",
            })
        });

        let (offset, page_size) = parse_cursor(params);
        let page_end = (offset + page_size).min(self.resources.len());
        let mut resources: Vec<serde_json::Value> = self.resources[offset..page_end]
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
        if offset == 0 {
            if let Some(entry) = card_entry {
                resources.insert(0, entry);
            }
        }

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

        // Expose the Server Card at the well-known URI. Matches the
        // HTTP convention (.well-known/mcp-card) but routed through
        // the stdio resource protocol.
        if uri == "well-known://mcp-card" {
            if let Some(ref card) = self.server_card {
                let content = serde_json::json!({
                    "uri": uri,
                    "text": serde_json::to_string(card).unwrap_or_else(|_| "{}".to_string()),
                    "mimeType": "application/json",
                });
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "contents": [content] }
                });
            }
        }

        // Static resources take precedence over templates.
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

        for tmpl in &self.resource_templates {
            if let Some(args) = match_uri_template(&tmpl.uri_template, uri) {
                let args_vm = json_to_vm_value(&serde_json::json!(args));
                let result = vm.call_closure_pub(&tmpl.handler, &[args_vm]).await;
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
        crate::jsonrpc::response(id.clone(), serde_json::json!({}))
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

        let result = vm.call_closure_pub(&prompt.handler, &[args_vm]).await;

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
