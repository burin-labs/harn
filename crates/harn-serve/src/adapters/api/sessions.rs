//! Session resource routes: list/create/get/update/close/fork/truncate
//! plus the session view projection, transcript messages, and the
//! session-scoped task list and submission entry points.

use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum SessionReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl SessionReasoningEffort {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionModelPolicy {
    provider: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<SessionReasoningEffort>,
}

#[derive(Clone, Debug)]
enum ModelPolicyChange {
    Unchanged,
    Clear,
    Set(SessionModelPolicy),
}

pub(super) async fn list_sessions(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    match state.session_resources() {
        Ok(sessions) => Json(list_response(sessions)).into_response(),
        Err(_) => task_store::unavailable_response(),
    }
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
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    if !input.is_object() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "session request must be a JSON object",
        );
    }
    let mode_id = match input.get("mode_id") {
        None | Some(Value::Null) => state.default_session_mode.as_str(),
        Some(Value::String(mode_id))
            if crate::adapters::acp::is_supported_session_mode(mode_id) =>
        {
            mode_id
        }
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_session_mode",
                "mode_id must be one of ask, architect, code, or shadow",
            )
        }
    };
    let model_policy = match parse_model_policy_change(&input, &state.provider_catalog) {
        Ok(ModelPolicyChange::Clear) => ModelPolicyChange::Unchanged,
        Ok(change) => change,
        Err(message) => {
            return api_error(StatusCode::BAD_REQUEST, "invalid_model_policy", &message)
        }
    };
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
                "cwd": workspace_root,
                // Stated rather than left to the server's default. This is the
                // hosted surface: its sessions never had the launcher's
                // environment, so naming `isolated` records the behaviour that
                // was already true instead of choosing a new one.
                "environmentPolicy": {"kind": "isolated", "grants": []},
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
    if mode_id != "ask" {
        if let Err(message) = state
            .acp
            .call(
                "session/set_mode",
                json!({"sessionId": session_id, "modeId": mode_id}),
            )
            .await
        {
            let _ = state
                .acp
                .call("session/close", json!({"sessionId": session_id}))
                .await;
            return api_error(StatusCode::BAD_GATEWAY, "acp_error", &message);
        }
    }
    if let Err(message) = apply_model_policy_change(&state, &session_id, &model_policy).await {
        return api_error(StatusCode::BAD_GATEWAY, "acp_error", &message);
    }
    let mut session = json!({
        "id": session_id,
        "object": "session",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "workspace_id": workspace_id,
        "state": "IDLE",
        "mode_id": mode_id,
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
    project_model_policy(&mut session, &model_policy);
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
    match state.session_resource(&session_id) {
        Ok(session) => Json(session).into_response(),
        Err(harn_session_store::StoreError::NotFound(_)) => {
            api_error(StatusCode::NOT_FOUND, "not_found", "session not found")
        }
        Err(_) => task_store::unavailable_response(),
    }
}

pub(super) async fn list_session_live_clients(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    if !api_session_exists(&state, &session_id) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    }
    match state
        .acp
        .call("session/live_clients", json!({"sessionId": session_id}))
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, "acp_error", &error),
    }
}

pub(super) async fn attach_session_client(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    live_client_mutation(state, session_id, uri, headers, body, "session/attach").await
}

pub(super) async fn takeover_session_client(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    live_client_mutation(state, session_id, uri, headers, body, "session/takeover").await
}

pub(super) async fn detach_session_client(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    live_client_mutation(state, session_id, uri, headers, body, "session/detach").await
}

pub(super) async fn heartbeat_session_client(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    live_client_mutation(state, session_id, uri, headers, body, "session/heartbeat").await
}

async fn live_client_mutation(
    state: ApiState,
    session_id: String,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    method: &'static str,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    if !api_session_exists(&state, &session_id) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let params = json!({
        "sessionId": session_id,
        "clientId": input.get("client_id").cloned().unwrap_or(Value::Null),
        "mode": input.get("mode").cloned().unwrap_or_else(|| json!("observer")),
        "takeover": input.get("takeover").cloned().unwrap_or(json!(false)),
        "promptInjection": input.get("prompt_injection").cloned().unwrap_or(Value::Null),
        "permissionRouting": input.get("permission_routing").cloned().unwrap_or(Value::Null),
        "reason": input.get("reason").cloned().unwrap_or(Value::Null),
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
    });
    match state.acp.call(method, params).await {
        Ok(result) => {
            project_live_client_state(&state, &session_id, &result);
            Json(result).into_response()
        }
        Err(error) => api_error(StatusCode::BAD_REQUEST, "live_client_error", &error),
    }
}

fn api_session_exists(state: &ApiState, session_id: &str) -> bool {
    let live = {
        state
            .inner
            .lock()
            .expect("api state poisoned")
            .sessions
            .contains_key(session_id)
    };
    live || state.session_descriptor(session_id).is_ok()
}

fn project_live_client_state(state: &ApiState, session_id: &str, result: &Value) {
    let mut inner = state.inner.lock().expect("api state poisoned");
    let Some(session) = inner.sessions.get_mut(session_id) else {
        return;
    };
    session["live_clients"] = result.get("clients").cloned().unwrap_or_else(|| json!([]));
    session["live_controller_id"] = result
        .get("active_controller_id")
        .cloned()
        .unwrap_or(Value::Null);
    session["updated_at"] = json!(now_rfc3339());
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
    let session = match state.session_resource(&session_id) {
        Ok(session) => session,
        Err(harn_session_store::StoreError::NotFound(_)) => {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found")
        }
        Err(_) => return task_store::unavailable_response(),
    };
    let history = match state.history(&EventFilter {
        session_id: Some(session_id),
        ..EventFilter::default()
    }) {
        Ok(history) => history,
        Err(_) => return task_store::unavailable_response(),
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
    let model_policy = match parse_model_policy_change(&input, &state.provider_catalog) {
        Ok(change) => change,
        Err(message) => {
            return api_error(StatusCode::BAD_REQUEST, "invalid_model_policy", &message)
        }
    };
    {
        let inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
    }
    if let Err(message) = apply_model_policy_change(&state, &session_id, &model_policy).await {
        return api_error(StatusCode::BAD_GATEWAY, "acp_error", &message);
    }
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let session = inner
            .sessions
            .get_mut(&session_id)
            .expect("session existence checked before policy update");
        merge_mutable_fields(session, &input, &["summary", "metadata"]);
        project_model_policy(session, &model_policy);
        session["updated_at"] = json!(now_rfc3339());
        session.clone()
    };
    state.append_event(Some(session_id), None, "session.updated", session.clone());
    Json(session).into_response()
}

fn parse_model_policy_change(
    input: &Value,
    provider_catalog: &ProviderCatalogRuntime,
) -> Result<ModelPolicyChange, String> {
    let Some(value) = input.get("model_policy") else {
        return Ok(ModelPolicyChange::Unchanged);
    };
    if value.is_null() {
        return Ok(ModelPolicyChange::Clear);
    }
    let mut policy: SessionModelPolicy = serde_json::from_value(value.clone()).map_err(|_| {
        "model_policy must contain provider, model, and optional reasoning_effort only".to_string()
    })?;
    policy.provider = policy.provider.trim().to_ascii_lowercase();
    policy.model = policy.model.trim().to_string();
    if policy.provider.is_empty() {
        return Err("model_policy.provider must not be empty".to_string());
    }
    if policy.model.is_empty() {
        return Err("model_policy.model must not be empty".to_string());
    }
    let provider_known = policy.provider == "mock"
        || provider_catalog
            .artifact()
            .providers
            .iter()
            .any(|provider| provider.id == policy.provider);
    if !provider_known {
        return Err(format!(
            "model_policy.provider '{}' is not registered",
            policy.provider
        ));
    }
    Ok(ModelPolicyChange::Set(policy))
}

pub(super) async fn restore_model_policy(
    state: &ApiState,
    session_id: &str,
    policy: Option<&Value>,
) -> Result<(), String> {
    let input = policy
        .map(|policy| json!({"model_policy": policy}))
        .unwrap_or_else(|| json!({}));
    let change = parse_model_policy_change(&input, &state.provider_catalog)?;
    apply_model_policy_change(state, session_id, &change).await
}

async fn apply_model_policy_change(
    state: &ApiState,
    session_id: &str,
    change: &ModelPolicyChange,
) -> Result<(), String> {
    let (model, reasoning_effort) = match change {
        ModelPolicyChange::Unchanged => return Ok(()),
        ModelPolicyChange::Clear => ("@inherit".to_string(), "@inherit"),
        ModelPolicyChange::Set(policy) => (
            format!("{}:{}", policy.provider, policy.model),
            policy
                .reasoning_effort
                .map(SessionReasoningEffort::as_str)
                .unwrap_or("@inherit"),
        ),
    };
    state
        .acp
        .call(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": "model",
                "value": model,
            }),
        )
        .await?;
    state
        .acp
        .call(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": "thought_level",
                "value": reasoning_effort,
            }),
        )
        .await?;
    Ok(())
}

fn project_model_policy(session: &mut Value, change: &ModelPolicyChange) {
    match change {
        ModelPolicyChange::Unchanged => {}
        ModelPolicyChange::Clear => {
            if let Some(session) = session.as_object_mut() {
                session.remove("model_policy");
            }
        }
        ModelPolicyChange::Set(policy) => {
            session["model_policy"] =
                serde_json::to_value(policy).expect("session model policy serializes");
        }
    }
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

struct SessionMessageSnapshot {
    task_view: harn_session_store::task::TaskMessageView,
    // Control-event order is shared by task and standalone message creation.
    entries: Vec<(String, Value, bool)>,
}

fn session_message_snapshot(
    state: &ApiState,
    session_id: &str,
) -> harn_session_store::StoreResult<SessionMessageSnapshot> {
    let store = state.task_store()?;
    let task_view = store.task_message_view(session_id)?;
    let inner = state.inner.lock().expect("api state poisoned");
    let messages = inner.messages.get(session_id);
    let event_ids: HashMap<_, _> = inner
        .events
        .iter()
        .filter(|event| {
            event.session_id.as_deref() == Some(session_id) && event.event == "message.created"
        })
        .filter_map(|event| event.payload["id"].as_str().map(|id| (id, &event.id)))
        .collect();
    let mut entries = Vec::new();
    if let Some(messages) = messages {
        for message in messages {
            let id = message["id"]
                .as_str()
                .and_then(|id| event_ids.get(id))
                .ok_or_else(|| {
                    harn_session_store::StoreError::Backend(
                        "message has no control event identity".into(),
                    )
                })?;
            entries.push(((*id).clone(), message.clone(), false));
        }
    }
    let live = messages.is_some();
    drop(event_ids);
    drop(inner);
    if !live {
        store.get_task_session(session_id)?;
    }
    entries.extend(
        task_view
            .events
            .iter()
            .map(|event| (event.id.clone(), event.payload.clone(), true)),
    );
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(SessionMessageSnapshot { task_view, entries })
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

    let snapshot = match session_message_snapshot(&state, &session_id) {
        Ok(snapshot) => snapshot,
        Err(_) => return task_store::unavailable_response(),
    };
    let retained_task_ids: Vec<_> = snapshot
        .entries
        .iter()
        .take(keep_first)
        .filter(|entry| entry.2)
        .map(|entry| entry.0.clone())
        .collect();
    if state
        .task_store()
        .and_then(|store| {
            store.retain_task_messages(
                &session_id,
                snapshot.task_view.through_event_id.as_deref(),
                &retained_task_ids,
            )
        })
        .is_err()
    {
        return task_store::unavailable_response();
    }
    let observed_messages: BTreeSet<_> = snapshot
        .entries
        .iter()
        .filter(|entry| !entry.2)
        .filter_map(|entry| entry.1["id"].as_str().map(str::to_owned))
        .collect();
    let retained_messages: BTreeSet<_> = snapshot
        .entries
        .iter()
        .take(keep_first)
        .filter(|entry| !entry.2)
        .filter_map(|entry| entry.1["id"].as_str().map(str::to_owned))
        .collect();

    let active_tasks = match state
        .task_store()
        .and_then(|store| store.list_tasks(Some(&session_id), None))
    {
        Ok(tasks) => tasks,
        Err(_) => return task_store::unavailable_response(),
    };
    for task in active_tasks
        .into_iter()
        .filter(|task| task.status.is_active())
    {
        if state
            .change_task(
                &task.id,
                harn_session_store::task::TaskStatus::Canceled,
                None,
            )
            .is_err()
        {
            return task_store::unavailable_response();
        }
    }
    let session = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        if let Some(messages) = inner.messages.get_mut(&session_id) {
            messages.retain(|message| {
                message["id"].as_str().is_some_and(|id| {
                    !observed_messages.contains(id) || retained_messages.contains(id)
                })
            });
        }
        let now = now_rfc3339();
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        };
        if session.get("state").and_then(Value::as_str) != Some("CLOSED") {
            session["state"] = json!("IDLE");
        }
        session["updated_at"] = json!(now);
        session.clone()
    };
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
    let messages = match session_message_snapshot(&state, &session_id) {
        Ok(snapshot) => snapshot.entries.into_iter().map(|entry| entry.1).collect(),
        Err(harn_session_store::StoreError::NotFound(_)) => {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found")
        }
        Err(_) => return task_store::unavailable_response(),
    };
    Json(list_response(limit_values(messages, query.limit))).into_response()
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
        let inner = state.inner.lock().expect("api state poisoned");
        if !inner.sessions.contains_key(&session_id) {
            return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
        }
    }
    if state
        .append_event_with_persistence(
            Some(session_id.clone()),
            None,
            "message.created",
            message.clone(),
            EventPersistence::SessionMessage,
        )
        .is_none()
    {
        return task_store::unavailable_response();
    }
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
    if !api_session_exists(&state, &session_id) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "session not found");
    }
    match state
        .ready_task_store()
        .and_then(|store| store.list_tasks(Some(&session_id), None))
    {
        Ok(tasks) => Json(list_response(limit_values(
            tasks
                .iter()
                .map(harn_session_store::task::TaskRecord::resource)
                .collect(),
            query.limit,
        )))
        .into_response(),
        Err(_) => task_store::unavailable_response(),
    }
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
