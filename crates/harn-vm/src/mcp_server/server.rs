use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::mcp_progress::{
    active_bus as active_progress_bus, install_active_bus as install_active_progress_bus,
    is_valid_progress_token, scope_context, ProgressBus, ProgressContext,
};
use crate::mcp_protocol::{
    self, apply_result_envelope, parse_request_metadata, server_discover_result, McpCacheHint,
};
use crate::stdlib::json_to_vm_value;
use crate::value::VmError;
use crate::vm::Vm;

use super::convert::{prompt_value_to_messages, vm_value_to_content, vm_value_to_json};
use super::defs::{
    McpCompletionSource, McpPromptDef, McpResourceDef, McpResourceTemplateDef, McpServerMetadata,
    McpToolDef,
};
use super::uri::{match_uri_template, uri_template_variables};

/// MCP server that exposes Harn tools, resources, and prompts over MCP JSON-RPC.
pub struct McpServer {
    server_name: String,
    server_version: String,
    tools: Vec<McpToolDef>,
    resources: Vec<McpResourceDef>,
    resource_templates: Vec<McpResourceTemplateDef>,
    prompts: Vec<McpPromptDef>,
    /// Optional Server Card payload — advertised by `server/discover` in
    /// `serverInfo.card` and exposed as a static
    /// resource at the well-known URI `well-known://mcp-card`.
    /// Populated by `harn serve mcp --card path/to/card.json`.
    server_card: Option<serde_json::Value>,
    instructions: Option<String>,
    connection: Mutex<mcp_protocol::McpServerSession>,
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
            server_card: None,
            instructions: None,
            connection: Mutex::new(mcp_protocol::McpServerSession::default()),
        }
    }

    /// Apply script-supplied server metadata from `mcp_server_metadata(...)`.
    pub fn with_metadata(mut self, metadata: McpServerMetadata) -> Self {
        if let Some(name) = metadata.name {
            self.server_name = name;
        }
        if let Some(version) = metadata.version {
            self.server_version = version;
        }
        self.instructions = metadata.instructions;
        self
    }

    /// Attach a Server Card to be advertised by `server/discover` and via
    /// the `well-known://mcp-card` resource. Call on a freshly-built
    /// `McpServer` before `run`.
    pub fn with_server_card(mut self, card: serde_json::Value) -> Self {
        self.server_card = Some(card);
        self
    }

    /// Run the stable MCP server loop over newline-delimited stdio.
    /// Client input is handled by `input_required` response/retry rounds,
    /// so the transport never has to demultiplex server-initiated requests.
    pub async fn run(&self, vm: &mut Vm) -> Result<(), VmError> {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let progress_bus = ProgressBus::from_mpsc(out_tx.clone());

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

        let previous_progress = install_active_progress_bus(Some(progress_bus));
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            // Stable MCP stdio is request/response. Stray responses have no
            // server-initiated request to match and are ignored.
            if msg.get("method").is_none() {
                continue;
            }
            if let Some(response) = self.handle_json_rpc(msg, vm).await {
                if out_tx.send(response).is_err() {
                    break;
                }
            }
        }

        // Closing out_tx tells the writer task to drain and exit.
        drop(out_tx);
        install_active_progress_bus(previous_progress);

        // Best-effort wait so the writer flushes any tail responses
        // before the function returns.
        let _ = writer.await;
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

        if method == "initialize" {
            let result = self
                .connection
                .lock()
                .expect("MCP session lock poisoned")
                .initialize(
                    &params,
                    serde_json::Value::Object(self.server_capabilities()),
                    self.server_info_value(),
                    self.instructions.as_deref(),
                );
            return Some(match result {
                Ok(result) => crate::jsonrpc::response(id, result),
                Err(error) => crate::jsonrpc::error_response(id, -32602, &error),
            });
        }

        let request_profile = match self
            .connection
            .lock()
            .expect("MCP session lock poisoned")
            .accept_request(&id, method, &params)
        {
            Ok(profile) => profile,
            Err(response) => return Some(response),
        };

        if let Some(response) =
            mcp_protocol::explicit_unsupported_method_response(id.clone(), method)
        {
            return Some(response);
        }

        let response = match method {
            mcp_protocol::METHOD_SERVER_DISCOVER => self.handle_server_discover(&id),
            "ping" => crate::jsonrpc::response(id.clone(), serde_json::json!({})),
            "harn.hitl.respond" => self.handle_hitl_respond(&id, &params).await,
            "tools/list" => self.handle_tools_list(&id, &params),
            "tools/call" => self.handle_tools_call(&id, &params, vm).await,
            mcp_protocol::METHOD_TASKS_GET => self.handle_task_lookup(&id, &params),
            mcp_protocol::METHOD_TASKS_UPDATE => self.handle_task_lookup(&id, &params),
            mcp_protocol::METHOD_TASKS_CANCEL => self.handle_task_lookup(&id, &params),
            "resources/list" => self.handle_resources_list(&id, &params),
            "resources/read" => self.handle_resources_read(&id, &params, vm).await,
            "resources/templates/list" => self.handle_resource_templates_list(&id, &params),
            "prompts/list" => self.handle_prompts_list(&id, &params),
            "prompts/get" => self.handle_prompts_get(&id, &params, vm).await,
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                self.handle_completion_complete(&id, &params, vm).await
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        };
        Some(if request_profile.uses_result_envelope() {
            apply_envelope(response, cache_hint_for_method(method))
        } else {
            response
        })
    }

    fn server_capabilities(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut capabilities = serde_json::Map::new();
        if !self.tools.is_empty() {
            capabilities.insert("tools".into(), serde_json::json!({}));
        }
        if !self.resources.is_empty()
            || !self.resource_templates.is_empty()
            || self.server_card.is_some()
        {
            capabilities.insert("resources".into(), serde_json::json!({}));
        }
        if !self.prompts.is_empty() {
            capabilities.insert("prompts".into(), serde_json::json!({}));
        }
        let mut extensions = mcp_protocol::tasks_capability();
        capabilities.insert("completions".into(), mcp_protocol::completions_capability());
        if self.resources.iter().any(|resource| {
            resource.uri.starts_with("ui://")
                && resource.mime_type.as_deref() == Some("text/html;profile=mcp-app")
        }) {
            extensions["io.modelcontextprotocol/ui"] = serde_json::json!({
                "mimeTypes": ["text/html;profile=mcp-app"]
            });
        }
        capabilities.insert("extensions".into(), extensions);
        capabilities
    }

    fn server_info_value(&self) -> serde_json::Value {
        let mut server_info = serde_json::json!({
            "name": self.server_name,
            "version": self.server_version
        });
        if let Some(ref card) = self.server_card {
            server_info["card"] = card.clone();
        }
        server_info
    }

    fn handle_server_discover(&self, id: &serde_json::Value) -> serde_json::Value {
        let capabilities = serde_json::Value::Object(self.server_capabilities());
        let result = server_discover_result(
            capabilities,
            self.server_info_value(),
            self.instructions.as_deref(),
        );
        crate::jsonrpc::response(id.clone(), result)
    }

    fn handle_tools_list(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let page = match mcp_protocol::mcp_list_page(params, self.tools.len(), "tools/list") {
            Ok(page) => page,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let tools: Vec<serde_json::Value> = self.tools[page.start..page.end]
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
                if let Some(ref icons) = t.icons {
                    entry["icons"] = icons.clone();
                }
                if let Some(ref meta) = t.meta {
                    entry["_meta"] = meta.clone();
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({ "tools": tools });
        if let Some(next_cursor) = page.next_cursor {
            result["nextCursor"] = serde_json::json!(next_cursor);
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

        // Bind a per-call progress context so the handler (and any
        // helpers it calls) can emit `notifications/progress` via
        // `mcp_report_progress(...)`. The context is wired only when
        // both the connection has a progress bus installed AND the
        // client opted in via `_meta.progressToken`. Using
        // `scope_context` (a tokio task-local) rather than a
        // thread-local guard keeps concurrent tool calls isolated even
        // when they share an OS thread via a `LocalSet`.
        let progress_token = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(is_valid_progress_token);
        let progress_ctx = progress_token
            .and_then(|token| active_progress_bus().map(|bus| ProgressContext::new(bus, token)));

        let result = match crate::mcp_input::scope_input_context(
            params,
            client_capabilities(params),
            scope_context(progress_ctx, vm.call_closure_pub(&tool.handler, &[args_vm])),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };

        match result {
            Ok(value) => {
                let content = vm_value_to_content(&value);
                let mut call_result = serde_json::json!({
                    "content": content,
                    "isError": false
                });
                if tool.output_schema.is_some() {
                    call_result["structuredContent"] = vm_value_to_json(&value);
                }
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": call_result
                })
            }
            Err(VmError::McpInputRequired(required)) => {
                crate::jsonrpc::response(id.clone(), crate::mcp_input::input_result(*required))
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": tool_error_text(&e) }],
                    "isError": true,
                },
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
        let mut all_resources = Vec::with_capacity(self.resources.len() + 1);
        if self.server_card.is_some() {
            all_resources.push(serde_json::json!({
                "uri": "well-known://mcp-card",
                "name": "Server Card",
                "description": "MCP v2.1 Server Card advertising this server's identity and capabilities",
                "mimeType": "application/json",
            }));
        }
        all_resources.extend(self.resources.iter().map(|r| {
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
            if let Some(ref meta) = r.meta {
                entry["_meta"] = meta.clone();
            }
            entry
        }));

        let page = match mcp_protocol::mcp_list_page(params, all_resources.len(), "resources/list")
        {
            Ok(page) => page,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let resources = all_resources[page.start..page.end].to_vec();

        let mut result = serde_json::json!({ "resources": resources });
        if let Some(next_cursor) = page.next_cursor {
            result["nextCursor"] = serde_json::json!(next_cursor);
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
            if let Some(ref meta) = resource.meta {
                content["_meta"] = meta.clone();
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
                let result = match crate::mcp_input::scope_input_context(
                    params,
                    client_capabilities(params),
                    vm.call_closure_pub(&tmpl.handler, &[args_vm]),
                )
                .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return crate::jsonrpc::error_response(id.clone(), -32602, &error)
                    }
                };
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
                    Err(VmError::McpInputRequired(required)) => crate::jsonrpc::response(
                        id.clone(),
                        crate::mcp_input::input_result(*required),
                    ),
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
        let page = match mcp_protocol::mcp_list_page(
            params,
            self.resource_templates.len(),
            "resources/templates/list",
        ) {
            Ok(page) => page,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let templates: Vec<serde_json::Value> = self.resource_templates[page.start..page.end]
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
        if let Some(next_cursor) = page.next_cursor {
            result["nextCursor"] = serde_json::json!(next_cursor);
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
        let page = match mcp_protocol::mcp_list_page(params, self.prompts.len(), "prompts/list") {
            Ok(page) => page,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let prompts: Vec<serde_json::Value> = self.prompts[page.start..page.end]
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
        if let Some(next_cursor) = page.next_cursor {
            result["nextCursor"] = serde_json::json!(next_cursor);
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
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

        let result = match crate::mcp_input::scope_input_context(
            params,
            client_capabilities(params),
            vm.call_closure_pub(&prompt.handler, &[args_vm]),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };

        match result {
            Ok(value) => {
                let messages = prompt_value_to_messages(&value);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "messages": messages }
                })
            }
            Err(VmError::McpInputRequired(required)) => {
                crate::jsonrpc::response(id.clone(), crate::mcp_input::input_result(*required))
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("{e}") }
            }),
        }
    }

    async fn handle_completion_complete(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let Some(ref_type) = params.pointer("/ref/type").and_then(|value| value.as_str()) else {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                "completion ref.type is required",
            );
        };
        match ref_type {
            "ref/prompt" => self.complete_prompt_argument(id, params, vm).await,
            "ref/resource" => {
                self.complete_resource_template_argument(id, params, vm)
                    .await
            }
            other => crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unsupported completion ref.type: {other}"),
            ),
        }
    }

    async fn complete_prompt_argument(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let name = params
            .pointer("/ref/name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let argument_name = match completion_argument_name(params) {
            Ok(name) => name,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let value = completion_argument_value(params);
        let prompt = match self.prompts.iter().find(|prompt| prompt.name == name) {
            Some(prompt) => prompt,
            None => {
                return crate::jsonrpc::error_response(
                    id.clone(),
                    -32602,
                    &format!("Unknown prompt: {name}"),
                );
            }
        };
        let Some(argument) = prompt
            .arguments
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|argument| argument.name == argument_name)
        else {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unknown prompt argument: {argument_name}"),
            );
        };
        let candidates =
            match completion_source_candidates(argument.completion.as_ref(), params, vm).await {
                Ok(candidates) => candidates,
                Err(error) => return crate::jsonrpc::error_response(id.clone(), -32603, &error),
            };
        crate::mcp_protocol::completion_result(id.clone(), candidates, value)
    }

    async fn complete_resource_template_argument(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let uri = params
            .pointer("/ref/uri")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let argument_name = match completion_argument_name(params) {
            Ok(name) => name,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let value = completion_argument_value(params);
        let template = match self
            .resource_templates
            .iter()
            .find(|template| template.uri_template == uri)
        {
            Some(template) => template,
            None => {
                return crate::jsonrpc::error_response(
                    id.clone(),
                    -32602,
                    &format!("Unknown resource template: {uri}"),
                );
            }
        };
        if !uri_template_variables(&template.uri_template)
            .iter()
            .any(|name| name == argument_name)
        {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unknown resource template argument: {argument_name}"),
            );
        }
        let candidates =
            match completion_source_candidates(template.completions.get(argument_name), params, vm)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => return crate::jsonrpc::error_response(id.clone(), -32603, &error),
            };
        crate::mcp_protocol::completion_result(id.clone(), candidates, value)
    }
}

/// Map a JSON-RPC method to its conservative cache hint. Read/list
/// methods get a TTL; everything else is `None`, which still routes
/// through [`apply_envelope`] so Stable clients see `resultType`.
fn cache_hint_for_method(method: &str) -> Option<&'static McpCacheHint> {
    const LIST: McpCacheHint = McpCacheHint::list_default();
    const READ: McpCacheHint = McpCacheHint::read_default();
    match method {
        "tools/list" | "resources/list" | "resources/templates/list" | "prompts/list" => {
            Some(&LIST)
        }
        "resources/read" => Some(&READ),
        _ => None,
    }
}

/// Stamp the stable `resultType`/cache-hint envelope onto a handler's
/// response in one place. Error responses pass through untouched.
fn tool_error_text(error: &VmError) -> String {
    match error {
        VmError::Thrown(value) => value.display(),
        other => format!("{other}"),
    }
}

fn apply_envelope(
    mut response: serde_json::Value,
    hint: Option<&'static McpCacheHint>,
) -> serde_json::Value {
    if let Some(result) = response.get_mut("result") {
        apply_result_envelope(result, hint);
    }
    response
}

fn client_capabilities(params: &serde_json::Value) -> serde_json::Value {
    serde_json::to_value(parse_request_metadata(params).client_capabilities())
        .unwrap_or_else(|_| serde_json::json!({}))
}

fn completion_argument_name(params: &serde_json::Value) -> Result<&str, String> {
    params
        .pointer("/argument/name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "completion argument.name is required".to_string())
}

fn completion_argument_value(params: &serde_json::Value) -> &str {
    params
        .pointer("/argument/value")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

async fn completion_source_candidates(
    source: Option<&McpCompletionSource>,
    params: &serde_json::Value,
    vm: &mut Vm,
) -> Result<Vec<String>, String> {
    let Some(source) = source else {
        return Ok(Vec::new());
    };
    let mut candidates = source.values.clone();
    if let Some(handler) = source.handler.as_ref() {
        let request = json_to_vm_value(params);
        let value = vm
            .call_closure_pub(handler, &[request])
            .await
            .map_err(|error| format!("{error}"))?;
        candidates.extend(completion_candidates_from_json(&vm_value_to_json(&value)));
    }
    Ok(candidates)
}

fn completion_candidates_from_json(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().filter_map(json_completion_string).collect()
        }
        serde_json::Value::Object(map) => map
            .get("values")
            .or_else(|| map.get("completion").and_then(|value| value.get("values")))
            .map(completion_candidates_from_json)
            .unwrap_or_default(),
        _ => json_completion_string(value).into_iter().collect(),
    }
}

fn json_completion_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}
