//! Task resource routes: submit (session-scoped or top-level), list,
//! get, and cancel. `submit_task_inner` is the shared submission core
//! that the session-scoped route delegates to.

use super::*;

pub(super) async fn submit_task(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    submit_task_inner(state, None, uri, headers, body).await
}

pub(super) async fn submit_task_inner(
    state: ApiState,
    path_session_id: Option<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let Some(session_id) = path_session_id.or_else(|| {
        input
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    }) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_session",
            "session_id is required",
        );
    };
    let (workspace_id, task, message_input) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let Some(session) = inner.sessions.get(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        let workspace_id = session
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or(&inner.root_workspace_id)
            .to_string();
        let task_id = format!("task_{}", Uuid::now_v7());
        let now = now_rfc3339();
        let input_value = input.get("input").cloned().unwrap_or_else(|| json!({}));
        let task = json!({
            "id": task_id,
            "object": "task",
            "created_at": now,
            "updated_at": now,
            "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
            "session_id": session_id,
            "workspace_id": workspace_id,
            "status": "WORKING",
            "input": input_value,
            "created_by": "api",
            "persona_id": input.get("persona_id").cloned().unwrap_or(Value::Null),
            "branch_id": input.get("branch_id").cloned().unwrap_or(Value::Null),
            "parent_task_id": input.get("parent_task_id").cloned().unwrap_or(Value::Null),
            "assigned_agent_id": null,
            "receipt_id": null,
            "outcome_id": null,
            "quota_id": null,
            "started_at": now,
            "completed_at": null,
            "canceled_at": null,
            "failure": null
        });
        inner.tasks.insert(
            task["id"].as_str().unwrap_or_default().to_string(),
            task.clone(),
        );
        inner.active_task_by_session.insert(
            task["session_id"].as_str().unwrap_or_default().to_string(),
            task["id"].as_str().unwrap_or_default().to_string(),
        );
        (workspace_id, task, input_value)
    };
    let task_id = task["id"].as_str().unwrap_or_default().to_string();
    let message = message_resource(&session_id, Some(&task_id), message_input.clone());
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner
            .messages
            .entry(session_id.clone())
            .or_default()
            .push(message);
    }
    state.append_event(
        Some(session_id.clone()),
        Some(task_id.clone()),
        "task.started",
        task.clone(),
    );
    let prompt = prompt_text(&message_input);
    let task_state = state.clone();
    tokio::spawn(async move {
        let result = match prompt {
            Some(prompt) => {
                task_state
                    .acp
                    .call(
                        "session/prompt",
                        json!({
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": prompt}]
                        }),
                    )
                    .await
            }
            None => Err("task input did not contain prompt text".to_string()),
        };
        let (status, event, payload) = match result {
            Ok(result) => ("COMPLETED", "task.completed", result),
            Err(error) => (
                "FAILED",
                "task.failed",
                json!({
                    "error": error
                }),
            ),
        };
        let mut task_snapshot = None;
        {
            let mut inner = task_state.inner.lock().expect("api state poisoned");
            if let Some(task) = inner.tasks.get_mut(&task_id) {
                if task.get("status").and_then(Value::as_str) != Some("CANCELED") {
                    let now = now_rfc3339();
                    task["status"] = json!(status);
                    task["updated_at"] = json!(&now);
                    if status == "COMPLETED" {
                        task["completed_at"] = json!(now);
                        task["outcome_id"] = json!(format!("outcome_{task_id}"));
                    } else {
                        task["failure"] = json!({
                            "code": "task_failed",
                            "message": payload.get("error").and_then(Value::as_str).unwrap_or("task failed")
                        });
                    }
                    task_snapshot = Some(task.clone());
                }
            }
            inner.active_task_by_session.remove(&session_id);
        }
        if let Some(task) = task_snapshot {
            task_state.append_event(Some(session_id), Some(task_id), event, task);
        }
    });
    let mut task = task;
    task["workspace_id"] = json!(workspace_id);
    (StatusCode::ACCEPTED, Json(task)).into_response()
}

pub(super) async fn list_tasks(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    let tasks = inner
        .tasks
        .values()
        .filter(|task| {
            query.workspace_id.as_deref().is_none_or(|workspace_id| {
                task.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
            }) && query.session_id.as_deref().is_none_or(|session_id| {
                task.get("session_id").and_then(Value::as_str) == Some(session_id)
            })
        })
        .cloned()
        .collect();
    Json(list_response(limit_values(tasks, query.limit))).into_response()
}

pub(super) async fn get_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.tasks.get(&task_id).cloned() {
        Some(task) => Json(task).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "task not found"),
    }
}

pub(super) async fn cancel_task(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body).await {
        return response;
    }
    let (session_id, task) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let (session_id, task_snapshot) = {
            let Some(task) = inner.tasks.get_mut(&task_id) else {
                return api_error(StatusCode::NOT_FOUND, "not_found", "task not found");
            };
            let session_id = task
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let now = now_rfc3339();
            task["status"] = json!("CANCELED");
            task["updated_at"] = json!(&now);
            task["canceled_at"] = json!(now);
            (session_id, task.clone())
        };
        inner.active_task_by_session.remove(&session_id);
        (session_id, task_snapshot)
    };
    state.acp.send_raw(
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}),
    );
    state.append_event(
        Some(session_id),
        Some(task_id),
        "task.canceled",
        task.clone(),
    );
    Json(task).into_response()
}
