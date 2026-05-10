use serde_json::{json, Value as JsonValue};

use harn_vm::mcp_protocol;

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

        if method == "initialize" {
            return self.handle_initialize(id, session, &params);
        }

        if request.get("id").is_none() {
            return JsonValue::Null;
        }

        if !session.initialized && method != "ping" {
            return harn_vm::jsonrpc::error_response(id, -32002, "server not initialized");
        }
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
            "tools/list" => self.handle_tools_list(id, &params),
            "tools/call" => self.handle_tools_call(id, session, &params).await,
            mcp_protocol::METHOD_TASKS_GET => self.handle_tasks_get(id, session, &params),
            mcp_protocol::METHOD_TASKS_RESULT => {
                self.handle_tasks_result(id, session, &params).await
            }
            mcp_protocol::METHOD_TASKS_LIST => self.handle_tasks_list(id, session, &params),
            mcp_protocol::METHOD_TASKS_CANCEL => self.handle_tasks_cancel(id, session, &params),
            "resources/list" => self.handle_resources_list(id, &params).await,
            "resources/read" => self.handle_resources_read(id, &params).await,
            "resources/subscribe" => self.handle_resources_subscribe(id, session, &params).await,
            "resources/unsubscribe" => self.handle_resources_unsubscribe(id, session, &params),
            "resources/templates/list" => self.handle_resource_templates_list(id, &params),
            "prompts/list" => self.handle_prompts_list(id, &params),
            "prompts/get" => self.handle_prompts_get(id, &params),
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

        harn_vm::jsonrpc::response(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": true },
                    "resources": { "listChanged": true, "subscribe": true },
                    "prompts": { "listChanged": true },
                    "logging": mcp_protocol::logging_capability(),
                    "tasks": mcp_protocol::tasks_capability(),
                    "completions": mcp_protocol::completions_capability(),
                },
                "serverInfo": {
                    "name": "harn-orchestrator",
                    "title": "Harn Orchestrator MCP",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Expose Harn trigger and orchestrator controls over MCP."
            }),
        )
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
