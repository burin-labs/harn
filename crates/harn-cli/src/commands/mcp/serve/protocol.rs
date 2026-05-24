use serde_json::{json, Value as JsonValue};

use harn_vm::mcp_protocol::{
    self, apply_rc_result_envelope, enforce_request_protocol_version, parse_request_metadata,
    server_discover_result, McpCacheHint, McpProtocolMode,
};

use super::types::{ConnectionState, McpOrchestratorService};
use super::MCP_PROTOCOL_VERSION;

impl McpOrchestratorService {
    pub(super) async fn handle_request(
        &self,
        session: &mut ConnectionState,
        request: JsonValue,
    ) -> JsonValue {
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        // Per-request RC negotiation: a Modern client tags every payload
        // with `_meta.io.modelcontextprotocol/protocolVersion`. We never
        // promote a connection to Modern based on initialize alone — the
        // RC explicitly forbids that — but a session may still receive
        // a mix of Legacy and Modern requests over its lifetime.
        let metadata = parse_request_metadata(&params);
        let request_mode = match enforce_request_protocol_version(&id, &metadata) {
            Ok(Some(mode)) => mode,
            Ok(None) => McpProtocolMode::Legacy,
            Err(response) => return response,
        };

        // server/discover and initialize are the two compatibility
        // probes: server/discover is the modern entry point, initialize
        // is the legacy one. Both must work without prior session state.
        if method == mcp_protocol::METHOD_SERVER_DISCOVER {
            session.protocol_mode = McpProtocolMode::Modern;
            // The orchestrator advertises itself as authenticated once
            // it has accepted a discover from a peer that survived the
            // HTTP-layer auth check; legacy clients still get the same
            // capability negotiation through `initialize`.
            session.authenticated = true;
            session.initialized = true;
            return self.handle_server_discover(id);
        }

        if method == "initialize" {
            return self.handle_initialize(id, session, &params);
        }

        if request.get("id").is_none() {
            return JsonValue::Null;
        }

        // RC clients can skip `initialize` entirely: any RC-tagged
        // request implicitly bootstraps the session and pins the
        // connection mode forward. Legacy clients still get the
        // original "server not initialized" guard.
        if request_mode.is_modern() {
            session.initialized = true;
            session.authenticated = true;
            session.protocol_mode = McpProtocolMode::Modern;
            session.protocol_version = mcp_protocol::DRAFT_PROTOCOL_VERSION.to_string();
        }

        if !session.initialized && method != "ping" {
            return harn_vm::jsonrpc::error_response(id, -32002, "server not initialized");
        }
        let mode = session.protocol_mode;

        if let Some(response) =
            mcp_protocol::unsupported_client_bound_method_response(id.clone(), method)
        {
            return response;
        }

        match method {
            "initialized" => JsonValue::Null,
            "ping" => harn_vm::jsonrpc::response(id, json!({})),
            mcp_protocol::METHOD_LOGGING_SET_LEVEL => {
                self.handle_logging_set_level(id, session, &params)
            }
            "tools/list" => apply_envelope(self.handle_tools_list(id, &params), mode, list_hint()),
            "tools/call" => apply_envelope(
                self.handle_tools_call(id, session, &params).await,
                mode,
                None,
            ),
            mcp_protocol::METHOD_TASKS_GET => self.handle_tasks_get(id, session, &params),
            mcp_protocol::METHOD_TASKS_RESULT => {
                self.handle_tasks_result(id, session, &params).await
            }
            mcp_protocol::METHOD_TASKS_LIST => self.handle_tasks_list(id, session, &params),
            mcp_protocol::METHOD_TASKS_CANCEL => self.handle_tasks_cancel(id, session, &params),
            "resources/list" => apply_envelope(
                self.handle_resources_list(id, &params).await,
                mode,
                list_hint(),
            ),
            "resources/read" => apply_envelope(
                self.handle_resources_read(id, &params).await,
                mode,
                read_hint(),
            ),
            "resources/subscribe" => self.handle_resources_subscribe(id, session, &params).await,
            "resources/unsubscribe" => self.handle_resources_unsubscribe(id, session, &params),
            "resources/templates/list" => apply_envelope(
                self.handle_resource_templates_list(id, &params),
                mode,
                list_hint(),
            ),
            "prompts/list" => {
                apply_envelope(self.handle_prompts_list(id, &params), mode, list_hint())
            }
            "prompts/get" => apply_envelope(self.handle_prompts_get(id, &params), mode, None),
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                self.handle_completion_complete(id, &params).await
            }
            _ if mcp_protocol::unsupported_latest_spec_method(method).is_some() => {
                mcp_protocol::unsupported_latest_spec_method_response(id, method)
                    .expect("checked unsupported MCP method")
            }
            _ => {
                harn_vm::jsonrpc::error_response(id, -32601, &format!("Method not found: {method}"))
            }
        }
    }

    pub(super) fn handle_server_discover(&self, id: JsonValue) -> JsonValue {
        harn_vm::jsonrpc::response(
            id,
            server_discover_result(
                orchestrator_capabilities(),
                orchestrator_server_info(),
                Some("Expose Harn trigger and orchestrator controls over MCP."),
            ),
        )
    }

    pub(super) fn handle_initialize(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let client_name = params
            .pointer("/clientInfo/name")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        let client_version = params
            .pointer("/clientInfo/version")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        session.client_identity = format!("{client_name}/{client_version}");
        session.protocol_version = params
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or(MCP_PROTOCOL_VERSION)
            .to_string();

        if super::http::initialize_api_key(params).is_some() {
            eprintln!(
                "[harn] warning: MCP initialize capabilities.harn.apiKey is deprecated; use HTTP Authorization: Bearer tokens with OAuth protected-resource metadata instead"
            );
        }

        if self.auth.has_api_keys() && !session.authenticated {
            let api_key = super::http::initialize_api_key(params);
            if api_key.is_none_or(|value| !self.auth.matches_api_key(value)) {
                return harn_vm::jsonrpc::error_response(id, -32001, "unauthorized");
            }
            session.authenticated = true;
        } else {
            session.authenticated = true;
        }
        session.initialized = true;
        session.protocol_mode = McpProtocolMode::Legacy;

        let mut result = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": orchestrator_capabilities(),
            "serverInfo": orchestrator_server_info(),
            "instructions": "Expose Harn trigger and orchestrator controls over MCP."
        });
        // Legacy initialize never carries `resultType`, so this leaves
        // the existing wire bytes unchanged. The branch keeps the path
        // symmetric in case a Modern client routes through here for
        // backwards-compat probing.
        apply_rc_result_envelope(&mut result, session.protocol_mode, None);
        harn_vm::jsonrpc::response(id, result)
    }

    pub(super) fn handle_prompts_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let prompts = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .list();
        paginated_list_response(id, "prompts/list", "prompts", params, prompts)
    }

    pub(super) fn handle_prompts_get(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .get(name, &arguments);
        match result {
            Ok(value) => harn_vm::jsonrpc::response(id, value),
            Err(error)
                if error.starts_with("Unknown prompt")
                    || error.starts_with("Missing required argument")
                    || error.starts_with("prompt arguments") =>
            {
                harn_vm::jsonrpc::error_response(id, -32602, &error)
            }
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    pub(super) fn handle_logging_set_level(
        &self,
        id: JsonValue,
        session: &mut ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let Some(level_str) = params.get("level").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "logging/setLevel requires params.level",
            );
        };
        let Some(level) = mcp_protocol::McpLogLevel::from_str_ci(level_str) else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                &format!("logging/setLevel: unsupported level '{level_str}'"),
            );
        };
        session.log_level = level;
        harn_vm::jsonrpc::response(id, json!({}))
    }

    pub(super) async fn handle_completion_complete(
        &self,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        let Some(ref_type) = params.pointer("/ref/type").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(id, -32602, "completion ref.type is required");
        };
        match ref_type {
            "ref/prompt" => self.handle_prompt_completion(id, params),
            "ref/resource" => self.handle_resource_completion(id, params).await,
            other => harn_vm::jsonrpc::error_response(
                id,
                -32602,
                &format!("Unsupported completion ref.type: {other}"),
            ),
        }
    }

    pub(super) fn handle_prompt_completion(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let name = params
            .pointer("/ref/name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let Some(argument_name) = params
            .pointer("/argument/name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "completion argument.name is required",
            );
        };
        let value = params
            .pointer("/argument/value")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let result = self
            .prompt_catalog
            .lock()
            .expect("prompt catalog poisoned")
            .complete(name, argument_name, value);
        match result {
            Ok(completion) => harn_vm::jsonrpc::response(id, json!({ "completion": completion })),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
        }
    }

    pub(super) async fn handle_resource_completion(
        &self,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        let uri_template = params
            .pointer("/ref/uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let Some(argument_name) = params
            .pointer("/argument/name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "completion argument.name is required",
            );
        };
        let value = params
            .pointer("/argument/value")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();

        let candidates = match (uri_template, argument_name) {
            ("harn://topic/{name}", "name") => match self.resource_template_topic_names().await {
                Ok(candidates) => candidates,
                Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
            },
            ("harn://event/{event_id}", "event_id") => {
                match self.resource_template_event_ids().await {
                    Ok(candidates) => candidates,
                    Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
                }
            }
            ("harn://dlq/{entry_id}", "entry_id") => {
                match self.resource_template_dlq_entry_ids().await {
                    Ok(candidates) => candidates,
                    Err(error) => return harn_vm::jsonrpc::error_response(id, -32603, &error),
                }
            }
            ("harn://topic/{name}", other)
            | ("harn://event/{event_id}", other)
            | ("harn://dlq/{entry_id}", other) => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!("Unknown resource template argument: {other}"),
                );
            }
            (other, _) => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!("Unknown resource template: {other}"),
                );
            }
        };

        harn_vm::jsonrpc::response(
            id,
            json!({
                "completion": mcp_protocol::completion_payload(candidates, value),
            }),
        )
    }

    pub(super) fn handle_resource_templates_list(
        &self,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        paginated_list_response(
            id,
            "resources/templates/list",
            "resourceTemplates",
            params,
            vec![
                json!({
                    "uriTemplate": "harn://topic/{name}",
                    "name": "topic",
                    "title": "EventLog Topic",
                    "description": "Read a Harn EventLog topic by name.",
                    "mimeType": "application/json",
                }),
                json!({
                    "uriTemplate": "harn://event/{event_id}",
                    "name": "trigger-event",
                    "title": "Trigger Event",
                    "description": "Read a recorded trigger event plus related replay and trace artifacts.",
                    "mimeType": "application/json",
                }),
                json!({
                    "uriTemplate": "harn://dlq/{entry_id}",
                    "name": "dead-letter-entry",
                    "title": "Dead-Letter Entry",
                    "description": "Read one pending dead-letter queue entry.",
                    "mimeType": "application/json",
                }),
            ],
        )
    }
}

/// Capabilities advertised by both `initialize` and `server/discover`.
/// Kept in a single place so the two paths cannot drift apart silently.
pub(super) fn orchestrator_capabilities() -> JsonValue {
    json!({
        "tools": { "listChanged": true },
        "resources": { "listChanged": true, "subscribe": true },
        "prompts": { "listChanged": true },
        "logging": mcp_protocol::logging_capability(),
        "tasks": mcp_protocol::tasks_capability(),
        "completions": mcp_protocol::completions_capability(),
    })
}

pub(super) fn orchestrator_server_info() -> JsonValue {
    json!({
        "name": "harn-orchestrator",
        "title": "Harn Orchestrator MCP",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn list_hint() -> Option<&'static McpCacheHint> {
    const LIST: McpCacheHint = McpCacheHint::list_default();
    Some(&LIST)
}

fn read_hint() -> Option<&'static McpCacheHint> {
    const READ: McpCacheHint = McpCacheHint::read_default();
    Some(&READ)
}

/// Stamp the RC `resultType`/cache-hint envelope onto a handler's
/// response in one place. Error responses pass through untouched —
/// the RC envelope only applies to `result` bodies.
fn apply_envelope(
    mut response: JsonValue,
    mode: McpProtocolMode,
    hint: Option<&'static McpCacheHint>,
) -> JsonValue {
    if let Some(result) = response.get_mut("result") {
        apply_rc_result_envelope(result, mode, hint);
    }
    response
}

pub(super) fn paginated_list_response(
    id: JsonValue,
    method: &str,
    result_key: &str,
    params: &JsonValue,
    items: Vec<JsonValue>,
) -> JsonValue {
    let page = match mcp_protocol::mcp_list_page(params, items.len(), method) {
        Ok(page) => page,
        Err(error) => return harn_vm::jsonrpc::error_response(id, -32602, &error),
    };
    let page_len = page.end - page.start;
    let page_items = items
        .into_iter()
        .skip(page.start)
        .take(page_len)
        .collect::<Vec<_>>();
    let mut result = serde_json::Map::new();
    result.insert(result_key.to_string(), JsonValue::Array(page_items));
    if let Some(next_cursor) = page.next_cursor {
        result.insert("nextCursor".to_string(), JsonValue::String(next_cursor));
    }
    harn_vm::jsonrpc::response(id, JsonValue::Object(result))
}
