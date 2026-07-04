//! Session resource routes: list/create/get/update/close/fork/truncate
//! plus the session view projection, transcript messages, and the
//! session-scoped task list and submission entry points.

use super::*;

pub(super) async fn list_sessions(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(inner.sessions.values().cloned().collect())).into_response()
}

pub(super) async fn create_session(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let input = parse_json_body(&body).unwrap_or_else(|_| json!({}));
    let (workspace_id, workspace_root) = {
        let inner = state.inner.lock().expect("api state poisoned");
        let workspace_id = input
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or(&inner.root_workspace_id)
            .to_string();
        let root = workspace_root_locked(&inner, &workspace_id)
            .unwrap_or_else(|| inner.root_workspace_path.clone());
        (workspace_id, root)
    };
    let result = match state
        .acp
        .call(
            "session/new",
            json!({
                "cwd": workspace_root
            }),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("session_{}", Uuid::now_v7()));
    let now = now_rfc3339();
    let session = json!({
        "id": session_id,
        "object": "session",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "workspace_id": workspace_id,
        "state": "IDLE",
        "transcript": {
            "source": "harn.acp",
            "session_id": session_id
        },
        "persona_id": input.get("persona_id").cloned().unwrap_or(Value::Null),
        "root_session_id": null,
        "parent_session_id": null,
        "branch_id": null,
        "last_event_id": null,
        "summary": null,
        "expires_at": null
    });
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.insert(session_id.clone(), session.clone());
        inner.messages.entry(session_id.clone()).or_default();
    }
    state.append_event(Some(session_id), None, "session.created", session.clone());
    (StatusCode::CREATED, Json(session)).into_response()
}

pub(super) async fn get_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.sessions.get(&session_id).cloned() {
        Some(session) => Json(session).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "session not found"),
    }
}

pub(super) async fn get_session_view(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let (session, history) = {
        let inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get(&session_id).cloned() else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        let history = inner
            .events
            .iter()
            .filter(|event| event.session_id.as_deref() == Some(session_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        (session, history)
    };
    Json(api_session_view(&session, &history)).into_response()
}

pub(super) async fn update_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PATCH, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        merge_mutable_fields(session, &input, &["summary", "metadata"]);
        session["updated_at"] = json!(now_rfc3339());
        session.clone()
    };
    state.append_event(Some(session_id), None, "session.updated", session.clone());
    Json(session).into_response()
}

pub(super) async fn close_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body).await {
        return response;
    }
    state.acp.send_raw(
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}),
    );
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        session["state"] = json!("CLOSED");
        session["updated_at"] = json!(now_rfc3339());
        session.clone()
    };
    state.append_event(Some(session_id), None, "session.closed", session.clone());
    Json(session).into_response()
}

pub(super) async fn fork_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let input = parse_json_body(&body).unwrap_or_else(|_| json!({}));
    let parent = {
        let inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.get(&session_id).cloned()
    };
    let Some(parent) = parent else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    };
    let result = match state
        .acp
        .call(
            "session/fork",
            json!({
                "sessionId": session_id,
                "branchName": input.get("branch_id").and_then(Value::as_str)
            }),
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let new_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("session_{}", Uuid::now_v7()));
    let now = now_rfc3339();
    let mut session = parent.clone();
    session["id"] = json!(new_id);
    session["created_at"] = json!(now);
    session["updated_at"] = json!(now);
    session["state"] = json!("IDLE");
    session["parent_session_id"] = json!(session_id);
    session["root_session_id"] = parent
        .get("root_session_id")
        .cloned()
        .filter(|value| !value.is_null())
        .unwrap_or_else(|| parent.get("id").cloned().unwrap_or(Value::Null));
    session["branch_id"] = input.get("branch_id").cloned().unwrap_or(Value::Null);
    session["metadata"] = input.get("metadata").cloned().unwrap_or_else(|| json!({}));
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner.sessions.insert(new_id.clone(), session.clone());
        inner.messages.entry(new_id.clone()).or_default();
    }
    state.append_event(Some(new_id), None, "session.forked", session.clone());
    (StatusCode::CREATED, Json(session)).into_response()
}

pub(super) async fn truncate_session(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let keep_first = match input
        .get("keep_first")
        .or_else(|| input.get("keepFirst"))
        .and_then(Value::as_i64)
    {
        Some(value) if value >= 0 => value as usize,
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "keep_first must be a non-negative integer",
            )
        }
    };
    {
        let inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
    }

    let mut acp_params = json!({
        "sessionId": session_id.clone(),
        "keepFirst": keep_first,
    });
    if let Some(reason) = input.get("reason").and_then(Value::as_str) {
        acp_params["reason"] = json!(reason);
    }
    let result = match state.acp.call("session/truncate", acp_params).await {
        Ok(result) => result,
        Err(error) => return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    };
    let kept_turn_count = result
        .get("keptTurnCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let removed_turn_count = result
        .get("removedTurnCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let new_tip_turn_id = result.get("newTipTurnId").cloned().unwrap_or(Value::Null);

    let (session, canceled_task) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        if let Some(messages) = inner.messages.get_mut(&session_id) {
            messages.truncate(keep_first);
        }
        let canceled_task_id = inner.active_task_by_session.remove(&session_id);
        let now = now_rfc3339();
        let canceled_task = canceled_task_id.and_then(|task_id| {
            let task = inner.tasks.get_mut(&task_id)?;
            if task.get("status").and_then(Value::as_str) != Some("CANCELED") {
                task["status"] = json!("CANCELED");
                task["updated_at"] = json!(&now);
                task["canceled_at"] = json!(&now);
            }
            Some((task_id, task.clone()))
        });
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        if session.get("state").and_then(Value::as_str) != Some("CLOSED") {
            session["state"] = json!("IDLE");
        }
        session["updated_at"] = json!(now);
        (session.clone(), canceled_task)
    };
    if let Some((task_id, task)) = canceled_task {
        state.append_event(
            Some(session_id.clone()),
            Some(task_id),
            "task.canceled",
            task,
        );
    }
    let response = json!({
        "object": "session.truncate_result",
        "session_id": session_id,
        "kept_turn_count": kept_turn_count,
        "removed_turn_count": removed_turn_count,
        "new_tip_turn_id": new_tip_turn_id,
        "session": session,
    });
    state.append_event(
        Some(session_id),
        None,
        "session.truncated",
        response.clone(),
    );
    Json(response).into_response()
}

pub(super) async fn list_session_messages(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    let Some(messages) = inner.messages.get(&session_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    };
    Json(list_response(limit_values(messages.clone(), query.limit))).into_response()
}

pub(super) async fn append_session_message(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let message_input = input.get("message").cloned().unwrap_or(input.clone());
    let message = message_resource(&session_id, None, message_input.clone());
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
        inner
            .messages
            .entry(session_id.clone())
            .or_default()
            .push(message.clone());
    }
    state.append_event(
        Some(session_id.clone()),
        None,
        "message.created",
        message.clone(),
    );
    if input.get("run").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(prompt) = prompt_text(&message_input) {
            let result = state
                .acp
                .call(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": prompt}]
                    }),
                )
                .await;
            if let Err(error) = result {
                return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error);
            }
        }
    }
    (StatusCode::CREATED, Json(message)).into_response()
}

pub(super) async fn list_session_tasks(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    if !inner.sessions.contains_key(&session_id) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    }
    let tasks = inner
        .tasks
        .values()
        .filter(|task| task.get("session_id").and_then(Value::as_str) == Some(session_id.as_str()))
        .cloned()
        .collect();
    Json(list_response(limit_values(tasks, query.limit))).into_response()
}

pub(super) async fn submit_session_task(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    tasks::submit_task_inner(state, Some(session_id), uri, headers, body).await
}

fn api_session_view(session: &Value, history: &[ApiEvent]) -> harn_vm::orchestration::SessionView {
    let session_id = session
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let last_event_id = history.iter().map(|event| event_seq(&event.id)).max();
    harn_vm::orchestration::build_session_view_from_run_views(
        Vec::new(),
        harn_vm::orchestration::SessionViewOptions {
            session_id,
            parent_session_id: session
                .get("parent_session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            root_session_id: session
                .get("root_session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            status: session
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("unknown".to_string())),
            started_at: session
                .get("created_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            updated_at: session
                .get("updated_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            last_event_id,
            event_count: history.len(),
            has_event_log: true,
            ..harn_vm::orchestration::SessionViewOptions::default()
        },
    )
}
