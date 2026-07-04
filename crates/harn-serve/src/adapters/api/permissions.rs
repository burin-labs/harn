//! Permission routes: list pending permission/HITL requests, respond to
//! them (bridging back into the ACP runtime and the permission store),
//! and manage the durable policy, remember-rules, audit history, and
//! ad-hoc permission checks.

use super::*;

pub(super) async fn list_permission_requests(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(limit_values(
        inner
            .permissions
            .values()
            .filter(|permission| {
                query.session_id.as_deref().is_none_or(|session_id| {
                    permission.public.get("session_id").and_then(Value::as_str) == Some(session_id)
                }) && query.task_id.as_deref().is_none_or(|task_id| {
                    permission.public.get("task_id").and_then(Value::as_str) == Some(task_id)
                })
            })
            .map(|permission| permission.public.clone())
            .collect(),
        query.limit,
    )))
    .into_response()
}

pub(super) async fn list_task_permission_requests(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(limit_values(
        inner
            .permissions
            .values()
            .filter(|permission| {
                permission.public.get("task_id").and_then(Value::as_str) == Some(task_id.as_str())
            })
            .map(|permission| permission.public.clone())
            .collect(),
        query.limit,
    )))
    .into_response()
}

pub(super) async fn respond_permission_request(
    State(state): State<ApiState>,
    AxumPath(request_id): AxumPath<String>,
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
    let approved = input
        .get("approved")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            matches!(
                input.get("outcome").and_then(Value::as_str),
                Some("approved" | "approve" | "selected")
            )
        });
    let outcome = if approved { "approved" } else { "denied" };
    let (permission, rpc_id, hitl) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let (permission, rpc_id, hitl, task_id) = {
            let Some(permission) = inner.permissions.get_mut(&request_id) else {
                return api_error(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "permission request not found",
                );
            };
            permission.public["status"] = json!(outcome);
            permission.public["updated_at"] = json!(now_rfc3339());
            permission.public["response"] = input.clone();
            let task_id = permission
                .public
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            (
                permission.public.clone(),
                permission.rpc_id,
                permission.hitl,
                task_id,
            )
        };
        if let Some(task_id) = task_id {
            set_task_status(&mut inner.tasks, &task_id, "WORKING");
        }
        (permission, rpc_id, hitl)
    };
    if hitl {
        let hitl_response = json!({
            "request_id": request_id,
            "approved": approved,
            "accepted": approved,
            "answer": input.get("answer").cloned().unwrap_or(Value::Null),
            "reviewer": input.get("reviewer").cloned().unwrap_or_else(|| json!("api")),
            "reason": input.get("reason").cloned().unwrap_or(Value::Null),
            "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({}))
        });
        if let Err(error) = state.acp.call("harn.hitl.respond", hitl_response).await {
            return api_error(StatusCode::BAD_GATEWAY, "acp_error", &error);
        }
    } else if let Some(rpc_id) = rpc_id {
        let result = if approved {
            json!({"outcome": {"outcome": "selected", "optionId": "allow"}})
        } else {
            json!({
                "outcome": {"outcome": "selected", "optionId": "reject"},
                "reason": input.get("reason").and_then(Value::as_str).unwrap_or("denied by API client")
            })
        };
        state.acp.send_raw(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": result
        }));
    }
    record_permission_response(&state, &permission, &input, approved).await;
    state.append_event(
        permission["session_id"].as_str().map(str::to_string),
        permission["task_id"].as_str().map(str::to_string),
        "permission.responded",
        permission.clone(),
    );
    Json(permission).into_response()
}

/// Bridge from the ACP-style `respond_permission_request` flow to the
/// new permissions store. Materializes a [`PermissionRequest`] from the
/// pending permission payload, records the audit entry, and optionally
/// installs a remember-rule when the responder asked to "remember"
/// their answer. The reconstruction is best-effort: ACP today does not
/// carry the full action/target shape, so missing fields fall back to
/// the public payload's `action` value or the literal "unknown" string.
async fn record_permission_response(
    state: &ApiState,
    permission: &Value,
    input: &Value,
    approved: bool,
) {
    let session_id = permission
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let action_value = permission.get("action").cloned().unwrap_or(Value::Null);
    let action = action_value
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            action_value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let target = action_value
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| action.clone());
    let class = input
        .get("class")
        .and_then(Value::as_str)
        .and_then(parse_action_class)
        .unwrap_or(ActionClass::Custom);
    let actor = input
        .get("reviewer")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "api".to_string());
    let mut request = PermissionRequest::new(
        permission
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        session_id,
        actor.clone(),
        class,
        action,
        target,
    );
    if let Some(reason) = input.get("reason").and_then(Value::as_str) {
        request.reason = Some(reason.to_string());
    }
    let policy_version = state.permissions.policy().await.version();
    let scope = input
        .get("scope")
        .and_then(Value::as_str)
        .and_then(parse_decision_scope)
        .unwrap_or(DecisionScope::Session);
    let expires_at = input
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|raw| OffsetDateTime::parse(raw, &Rfc3339).ok());
    let decision = if approved {
        PermissionDecision::Granted {
            scope,
            policy_version,
            reason: request.reason.clone(),
            expires_at,
            rule_id: None,
        }
    } else {
        PermissionDecision::Denied {
            scope,
            policy_version,
            reason: request.reason.clone(),
            rule_id: None,
        }
    };
    let remember = input
        .get("remember")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| RememberSpec {
            scope,
            action_pattern: input
                .get("action_pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            target_pattern: input
                .get("target_pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at,
        });
    state
        .permissions
        .record_decision(&request, &decision, Some(actor), remember)
        .await;
}

fn parse_action_class(raw: &str) -> Option<ActionClass> {
    match raw {
        "read" => Some(ActionClass::Read),
        "write" => Some(ActionClass::Write),
        "exec" => Some(ActionClass::Exec),
        "net" => Some(ActionClass::Net),
        "llm" => Some(ActionClass::Llm),
        "custom" => Some(ActionClass::Custom),
        _ => None,
    }
}

fn parse_decision_scope(raw: &str) -> Option<DecisionScope> {
    match raw {
        "session" => Some(DecisionScope::Session),
        "workspace" => Some(DecisionScope::Workspace),
        "user" => Some(DecisionScope::User),
        "always" => Some(DecisionScope::Always),
        _ => None,
    }
}

pub(super) async fn get_permissions_policy(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let policy = state.permissions.policy().await;
    let version = policy.version();
    Json(json!({
        "object": "permission_policy",
        "version": version.as_str(),
        "policy": policy,
    }))
    .into_response()
}

pub(super) async fn put_permissions_policy(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PUT, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let policy_value = input.get("policy").cloned().unwrap_or(input.clone());
    let policy: PermissionPolicy = match serde_json::from_value(policy_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_policy",
                &format!("policy did not deserialize: {error}"),
            );
        }
    };
    if let Err(errors) = policy.lint() {
        let messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_policy",
            &messages.join("; "),
        );
    }
    let version = state.permissions.install_policy(policy.clone()).await;
    Json(json!({
        "object": "permission_policy",
        "version": version.as_str(),
        "policy": policy,
    }))
    .into_response()
}

pub(super) async fn list_permission_rules(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let rules = state.permissions.rules().await;
    let data: Vec<Value> = rules
        .into_iter()
        .map(|rule| serde_json::to_value(rule).unwrap_or(Value::Null))
        .collect();
    Json(list_response(data)).into_response()
}

pub(super) async fn create_permission_rule(
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
    let rule: RememberRule = match serde_json::from_value(input) {
        Ok(rule) => rule,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_rule",
                &format!("rule did not deserialize: {error}"),
            );
        }
    };
    state.permissions.add_rule(rule.clone()).await;
    Json(serde_json::to_value(&rule).unwrap_or(Value::Null)).into_response()
}

pub(super) async fn revoke_permission_rule(
    State(state): State<ApiState>,
    AxumPath(rule_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::DELETE, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let id = RuleId(rule_id);
    if state.permissions.revoke_rule(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "not_found", "rule not found")
    }
}

#[derive(Deserialize, Default)]
pub(super) struct PermissionHistoryQuery {
    session_id: Option<String>,
    workspace_id: Option<String>,
    tenant_id: Option<String>,
    actor: Option<String>,
    outcome: Option<crate::permissions::AuditOutcome>,
    limit: Option<usize>,
}

pub(super) async fn get_permission_history(
    State(state): State<ApiState>,
    Query(query): Query<PermissionHistoryQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = AuditFilter {
        tenant_id: query.tenant_id,
        session_id: query.session_id,
        workspace_id: query.workspace_id,
        actor: query.actor,
        outcome: query.outcome,
        since: None,
        limit: query.limit,
    };
    let entries = state.permissions.history(&filter).await;
    let data: Vec<Value> = entries
        .into_iter()
        .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
        .collect();
    Json(list_response(data)).into_response()
}

pub(super) async fn check_permission(
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
    let mut request: PermissionRequest = match serde_json::from_value(input) {
        Ok(request) => request,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("permission request did not deserialize: {error}"),
            );
        }
    };
    if request.id.is_empty() {
        request.id = format!("permission_{}", Uuid::now_v7());
    }
    let decision = state.permissions.evaluate(&request).await;
    state
        .permissions
        .record_decision(&request, &decision, None, None)
        .await;
    Json(json!({
        "object": "permission_decision",
        "request_id": request.id,
        "decision": decision,
    }))
    .into_response()
}
