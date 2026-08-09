use serde_json::{json, Value as JsonValue};

use harn_vm::mcp_protocol::{self, apply_result_envelope, server_discover_result, McpCacheHint};

use super::types::{ConnectionState, McpOrchestratorService};
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
            return match session.mcp.initialize(
                &params,
                orchestrator_capabilities(),
                orchestrator_server_info(),
                Some("Expose Harn trigger and orchestrator controls over MCP."),
            ) {
                Ok(result) => {
                    session.authenticated = true;
                    harn_vm::jsonrpc::response(id, result)
                }
                Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
            };
        }

        if request.get("id").is_none() && method == "notifications/initialized" {
            return JsonValue::Null;
        }

        let request_profile = match session.mcp.accept_request(&id, method, &params) {
            Ok(profile) => profile,
            Err(response) => return response,
        };

        if method == mcp_protocol::METHOD_SERVER_DISCOVER {
            session.authenticated = true;
            return self.handle_server_discover(id);
        }

        if request.get("id").is_none() {
            return JsonValue::Null;
        }

        session.authenticated = true;

        if let Some(response) =
            mcp_protocol::explicit_unsupported_method_response(id.clone(), method)
        {
            return response;
        }

        let response = match method {
            "ping" => harn_vm::jsonrpc::response(id, json!({})),
            "tools/list" => self.handle_tools_list(id, &params),
            // Tool execution is the deepest request branch (trigger dispatch
            // can enter a child VM and agent machinery). Keep that state
            // machine behind one pointer instead of embedding it in the
            // already broad protocol dispatcher frame.
            "tools/call" => Box::pin(self.handle_tools_call(id, session, &params)).await,
            mcp_protocol::METHOD_TASKS_GET => self.handle_tasks_get(id, session, &params),
            mcp_protocol::METHOD_TASKS_UPDATE => self.handle_tasks_update(id, session, &params),
            mcp_protocol::METHOD_TASKS_CANCEL => self.handle_tasks_cancel(id, session, &params),
            "resources/list" => self.handle_resources_list(id, &params).await,
            "resources/read" => self.handle_resources_read(id, &params).await,
            "resources/templates/list" => self.handle_resource_templates_list(id, &params),
            "prompts/list" => self.handle_prompts_list(id, &params),
            "prompts/get" => self.handle_prompts_get(id, &params),
            mcp_protocol::METHOD_COMPLETION_COMPLETE => {
                self.handle_completion_complete(id, &params).await
            }
            _ => {
                harn_vm::jsonrpc::error_response(id, -32601, &format!("Method not found: {method}"))
            }
        };
        if request_profile.uses_result_envelope() {
            apply_envelope(response, cache_hint_for_method(method))
        } else {
            response
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

/// Capabilities advertised by `server/discover`.
pub(super) fn orchestrator_capabilities() -> JsonValue {
    json!({
        "tools": {},
        "resources": {},
        "prompts": {},
        "extensions": mcp_protocol::tasks_capability(),
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

/// Map a JSON-RPC method to its conservative cache hint. Read/list
/// methods get a TTL; everything else is `None`, which still routes
/// through [`apply_envelope`] so Stable clients see `resultType`.
pub(super) fn cache_hint_for_method(method: &str) -> Option<&'static McpCacheHint> {
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
/// response in one place. Error responses pass through untouched —
/// the stable envelope only applies to `result` bodies.
pub(super) fn apply_envelope(
    mut response: JsonValue,
    hint: Option<&'static McpCacheHint>,
) -> JsonValue {
    if let Some(result) = response.get_mut("result") {
        apply_result_envelope(result, hint);
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
