//! Meta and introspection routes: health, version, OpenAPI document,
//! API root index, runtime facts, provider catalog, capability summary,
//! and the local control-plane tool registry.

use super::*;

pub(super) async fn health() -> Response {
    Json(json!({
        "ok": true,
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
    .into_response()
}

pub(super) async fn version() -> Response {
    Json(json!({
        "object": "version",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION
    }))
    .into_response()
}

pub(super) async fn openapi_json() -> Response {
    match serde_yml::from_str::<Value>(OPENAPI_YAML) {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "openapi_parse_failed",
            &error.to_string(),
        ),
    }
}

pub(super) async fn api_root() -> Response {
    Json(json!({
        "object": "api",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION,
        "openapi": "/openapi.json",
        "resources": {
            "runtime": "/v1/runtime",
            "capabilities": "/v1/capabilities",
            "provider_catalog": "/v1/provider-catalog",
            "tools": "/v1/tools",
            "workspaces": "/v1/workspaces",
            "sessions": "/v1/sessions",
            "session_view": "/v1/sessions/{session_id}/view",
            "tasks": "/v1/tasks",
            "artifacts": "/v1/artifacts",
            "events": "/v1/events/stream",
            "workflow_trigger_runs": "/v1/workflow-trigger-runs",
            "permission_requests": "/v1/permission-requests",
            "permission_policy": "/v1/permissions/policy",
            "permission_rules": "/v1/permissions/rules",
            "permission_history": "/v1/permissions/history",
            "permission_check": "/v1/permissions/check"
        }
    }))
    .into_response()
}

pub(super) async fn runtime(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(json!({
        "object": "runtime",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": API_PROTOCOL_VERSION,
        "adapter": "harn-serve-api",
        "workspace_root": inner.root_workspace_path,
        "session_count": inner.sessions.len(),
        "task_count": inner.tasks.len(),
        "capabilities": capability_values()
    }))
    .into_response()
}

pub(super) async fn provider_catalog(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    Json(state.provider_catalog.artifact()).into_response()
}

pub(super) async fn capabilities(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    Json(json!({
        "object": "capability_summary",
        "capabilities": capability_values()
    }))
    .into_response()
}

pub(super) async fn list_tools(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    Json(list_response(tool_values())).into_response()
}

pub(super) async fn get_tool(
    State(state): State<ApiState>,
    AxumPath(tool_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let Some(tool) = tool_values()
        .into_iter()
        .find(|tool| tool.get("id").and_then(Value::as_str) == Some(tool_id.as_str()))
    else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "tool not found");
    };
    Json(tool).into_response()
}

fn capability_values() -> Vec<Value> {
    vec![
        json!({"id": "sessions", "description": "Create, inspect, fork, truncate, update, and close ACP-backed Harn sessions."}),
        json!({"id": "tasks", "description": "Submit prompts asynchronously, track task status, and abort active tasks."}),
        json!({"id": "artifacts", "description": "Register, inspect, list, and safely download local file artifacts."}),
        json!({"id": "events", "description": "Read snapshots and stream live session, task, tool, permission, and runtime events over SSE."}),
        json!({"id": "workflow_trigger_runs", "description": "Read recent Harn trigger dispatches and joined action-graph observations for local workflow operators."}),
        json!({"id": "permissions", "description": "Approve or deny host permission and HITL requests through the same ACP runtime path."}),
        json!({"id": "provider_catalog", "description": "Read the normalized Harn provider/model catalog used by this runtime."}),
        json!({"id": "tools", "description": "Inspect the local control-plane tool registry exposed by this server."}),
        json!({"id": "workspace.files", "description": "Read and write UTF-8 workspace files under registered workspace roots."}),
    ]
}

fn tool_values() -> Vec<Value> {
    vec![
        json!({
            "id": "harn.session.prompt",
            "object": "tool",
            "name": "session.prompt",
            "description": "Submit text to a Harn session through ACP session/prompt.",
            "input_schema": {"type": "object", "required": ["session_id", "text"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.session.cancel",
            "object": "tool",
            "name": "session.cancel",
            "description": "Cancel the active prompt for a Harn session.",
            "input_schema": {"type": "object", "required": ["session_id"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.session.truncate",
            "object": "tool",
            "name": "session.truncate",
            "description": "Drop a Harn session transcript after the first N turns.",
            "input_schema": {"type": "object", "required": ["session_id", "keep_first"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.permission.respond",
            "object": "tool",
            "name": "permission.respond",
            "description": "Approve or deny ACP permission and HITL requests.",
            "input_schema": {"type": "object", "required": ["request_id", "approved"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.events.stream",
            "object": "tool",
            "name": "events.stream",
            "description": "Stream Harn local API events as Server-Sent Events.",
            "input_schema": {"type": "object"},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.artifact.register",
            "object": "tool",
            "name": "artifact.register",
            "description": "Register durable artifact metadata or a mediated file URI.",
            "input_schema": {"type": "object", "required": ["kind", "mime_type", "visibility"]},
            "output_schema": {"type": "object"}
        }),
        json!({
            "id": "harn.workspace.file",
            "object": "tool",
            "name": "workspace.file",
            "description": "Read or write UTF-8 workspace files below a registered root.",
            "input_schema": {"type": "object", "required": ["workspace_id", "path"]},
            "output_schema": {"type": "object"}
        }),
    ]
}
