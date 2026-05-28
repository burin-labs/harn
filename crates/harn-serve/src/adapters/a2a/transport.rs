//! HTTP, REST, JSON-RPC, and SSE routing for A2A.
use super::schema::*;
use super::*;

impl A2aServer {
    pub async fn run_http(self: Arc<Self>, options: A2aHttpServeOptions) -> Result<(), String> {
        let listener = crate::tls::bind_listener(options.bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        let public_url = options.public_url.unwrap_or_else(|| {
            format!(
                "{}://localhost:{}",
                options.tls.advertised_scheme(),
                local_addr.port()
            )
        });
        let state = HttpState {
            server: self,
            public_url: public_url.clone(),
        };
        let router = Self::http_router(state);
        let router = crate::tls::apply_security_headers(router, &options.tls);

        eprintln!("Harn A2A server listening on {public_url}");
        eprintln!("[harn] A2A workflow server ready on {public_url}");
        eprintln!("[harn] Agent card: {public_url}{A2A_AGENT_CARD_PATH}");
        crate::tls::serve_router_from_tcp(listener, router, &options.tls)
            .await
            .map_err(|error| format!("A2A HTTP server failed: {error}"))
    }

    pub(super) fn http_router(state: HttpState) -> Router {
        Router::new()
            .route("/", post(jsonrpc_request))
            .route(A2A_AGENT_CARD_PATH, get(agent_card_request))
            .route("/agent/card", get(agent_card_request))
            .route("/.well-known/a2a-agent", get(agent_card_request))
            .route("/.well-known/agent.json", get(agent_card_request))
            // Canonical A2A 0.3.0 HTTP+JSON/REST binding.
            .route("/v1/message:send", post(rest_v1_message_send))
            .route("/v1/message:stream", post(rest_v1_message_stream))
            .route("/v1/card", get(rest_v1_card))
            .route(
                "/v1/tasks/{id_action}",
                get(rest_v1_get_task).post(rest_v1_task_action),
            )
            .route(
                "/v1/tasks/{id}/pushNotificationConfigs",
                post(rest_v1_push_config_set).get(rest_v1_push_config_list),
            )
            .route(
                "/v1/tasks/{id}/pushNotificationConfigs/{config_id}",
                get(rest_v1_push_config_get).delete(rest_v1_push_config_delete),
            )
            // Legacy non-canonical REST aliases (deprecated; will be
            // removed one minor cycle after the canonical /v1 surface ships).
            .route("/message/send", post(rest_legacy_message_send))
            .route("/message/stream", post(rest_legacy_message_stream))
            .route("/tasks/send", post(rest_legacy_send_task))
            .route("/tasks/send_and_wait", post(rest_legacy_send_and_wait_task))
            .route("/tasks/cancel", post(rest_legacy_cancel_task))
            .route("/tasks/resubscribe", post(rest_legacy_resubscribe_task))
            .layer(axum::extract::DefaultBodyLimit::max(
                crate::DEFAULT_HTTP_BODY_LIMIT_BYTES,
            ))
            .with_state(state)
    }

    #[cfg(test)]
    pub(super) async fn process_rpc(
        self: Arc<Self>,
        request: JsonValue,
        auth: AuthRequest,
    ) -> ProcessedRpc {
        self.process_rpc_with_public_url(request, auth, "http://localhost:8080")
            .await
    }

    pub(super) async fn process_rpc_with_public_url(
        self: Arc<Self>,
        request: JsonValue,
        auth: AuthRequest,
        public_url: &str,
    ) -> ProcessedRpc {
        let rpc_id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        let principal = match self.authorize_protocol_request(rpc_id.clone(), &auth).await {
            Ok(principal) => principal,
            Err(processed) => return processed,
        };
        let (outcome, deprecation) = match method {
            "message/send" | "a2a.SendMessage" | "tasks/send" | "tasks/send_and_wait" => {
                let deprecation = match method {
                    "a2a.SendMessage" | "tasks/send" | "tasks/send_and_wait" => {
                        Some("Use A2A 0.3.0 method `message/send`.")
                    }
                    _ => None,
                };
                let wait = if method == "tasks/send" {
                    false
                } else if method == "tasks/send_and_wait" {
                    true
                } else {
                    !return_immediately(&params)
                };
                match self.prepare_task(&params, auth).await {
                    Ok(task) if wait => {
                        self.run_task_to_completion(&task).await;
                        (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task.id))),
                            deprecation,
                        )
                    }
                    Ok(task) => {
                        let task_id = task.id.clone();
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task_id))),
                            deprecation,
                        )
                    }
                    Err(error) => {
                        return prepare_error_to_processed(error, rpc_id, deprecation);
                    }
                }
            }
            "message/stream" | "a2a.SendStreamingMessage" | "tasks/sendSubscribe" => {
                let deprecation = match method {
                    "a2a.SendStreamingMessage" | "tasks/sendSubscribe" => {
                        Some("Use A2A 0.3.0 method `message/stream`.")
                    }
                    _ => None,
                };
                match self.prepare_task(&params, auth).await {
                    Ok(task) => {
                        let rx = self.subscribe(&task.id).unwrap_or_else(empty_stream);
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        (RpcOutcome::Sse(rx), deprecation)
                    }
                    Err(error) => {
                        return prepare_error_to_processed(error, rpc_id, deprecation);
                    }
                }
            }
            "tasks/resubscribe" | "a2a.ResubscribeTask" => {
                let deprecation = (method == "a2a.ResubscribeTask")
                    .then_some("Use A2A 0.3.0 method `tasks/resubscribe`.");
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.subscribe(id)) {
                    Some(rx) => (RpcOutcome::Sse(rx), deprecation),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                }
            }
            "a2a.GetTask" | "tasks/get" => {
                let deprecation =
                    (method == "a2a.GetTask").then_some("Use A2A 0.3.0 method `tasks/get`.");
                let task_id = task_id_param(&params);
                match task_id.map(|id| self.task_json(id)) {
                    Some(JsonValue::Null) | None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                    Some(task) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                        deprecation,
                    ),
                }
            }
            "a2a.CancelTask" | "tasks/cancel" => {
                let deprecation =
                    (method == "a2a.CancelTask").then_some("Use A2A 0.3.0 method `tasks/cancel`.");
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.cancel_task(id).ok()) {
                    Some(task) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                        deprecation,
                    ),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_CANCELABLE,
                            "Task not cancelable",
                        )),
                        deprecation,
                    ),
                }
            }
            "a2a.ListTasks" | "tasks/list" => (
                RpcOutcome::Json(task_rpc_response(&rpc_id, self.list_tasks())),
                Some("`tasks/list` is a Harn compatibility method and is not part of A2A 0.3.0."),
            ),
            "CreateTaskPushNotificationConfig" | "tasks/pushNotificationConfig/set" => {
                let deprecation = (method == "CreateTaskPushNotificationConfig")
                    .then_some("Use A2A 0.3.0 method `tasks/pushNotificationConfig/set`.");
                let task_id = task_id_param(&params);
                let config = params
                    .get("pushNotificationConfig")
                    .or_else(|| params.get("config"))
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                match task_id {
                    Some(id) => match self.add_push_config(id, config).await {
                        Ok(config) => (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, config)),
                            deprecation,
                        ),
                        Err(error) => (
                            RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                            deprecation,
                        ),
                    },
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                }
            }
            "tasks/pushNotificationConfig/get" => {
                let task_id = task_id_param(&params);
                let config_id = push_config_id_param(&params);
                match task_id.and_then(|id| self.push_config(id, config_id).ok()) {
                    Some(config) => (RpcOutcome::Json(task_rpc_response(&rpc_id, config)), None),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        None,
                    ),
                }
            }
            "tasks/pushNotificationConfig/list" => {
                let task_id = task_id_param(&params);
                match self.push_configs(task_id) {
                    Ok(configs) => (RpcOutcome::Json(task_rpc_response(&rpc_id, configs)), None),
                    Err(error) => (
                        RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                        None,
                    ),
                }
            }
            "tasks/pushNotificationConfig/delete" => {
                let task_id = task_id_param(&params);
                let config_id = push_config_id_param(&params);
                match task_id.zip(config_id) {
                    Some((task_id, config_id)) => {
                        match self.delete_push_config(task_id, config_id).await {
                            Ok(()) => (
                                RpcOutcome::Json(task_rpc_response(&rpc_id, JsonValue::Null)),
                                None,
                            ),
                            Err(error) => (
                                RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                                None,
                            ),
                        }
                    }
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        None,
                    ),
                }
            }
            "agent/getAuthenticatedExtendedCard" => {
                let policy = self.core.auth_policy();
                if policy.methods.is_empty() {
                    (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED,
                            "ExtendedAgentCardNotConfiguredError: agent has no authentication methods configured",
                        )),
                        None,
                    )
                } else {
                    let subject = principal
                        .as_ref()
                        .map(|principal| principal.subject.as_str())
                        .unwrap_or("authenticated");
                    (
                        RpcOutcome::Json(task_rpc_response(
                            &rpc_id,
                            self.extended_agent_card(public_url, subject),
                        )),
                        None,
                    )
                }
            }
            _ => (
                RpcOutcome::Json(error_response(
                    rpc_id,
                    A2A_UNSUPPORTED_OPERATION,
                    &format!("UnsupportedOperationError: {method}"),
                )),
                None,
            ),
        };
        ProcessedRpc {
            outcome,
            deprecation,
            status: None,
            auth_challenge: None,
        }
    }

    async fn authorize_protocol_request(
        &self,
        rpc_id: JsonValue,
        auth: &AuthRequest,
    ) -> Result<Option<crate::AuthenticatedPrincipal>, ProcessedRpc> {
        let policy = self.core.auth_policy();
        if policy.methods.is_empty() {
            return Ok(None);
        }
        match policy.authorize(auth).await {
            AuthorizationDecision::Authorized(principal) => Ok(Some(principal)),
            AuthorizationDecision::Rejected(message) => Err(ProcessedRpc {
                outcome: RpcOutcome::Json(error_response(
                    rpc_id,
                    -32000,
                    &format!("Unauthorized: {message}"),
                )),
                deprecation: None,
                status: Some(StatusCode::UNAUTHORIZED),
                auth_challenge: Some(www_authenticate_header(policy)),
            }),
            // The protocol-level path passes no per-route scopes, so this
            // arm is unreachable by construction. Per-route scope checks
            // happen later when dispatch invokes the function and surface
            // as `DispatchError::Forbidden`. Wire it defensively here in
            // case future code paths feed scopes into this call.
            AuthorizationDecision::MissingScope { required, granted } => Err(ProcessedRpc {
                outcome: RpcOutcome::Json(error_response(
                    rpc_id,
                    -32003,
                    &crate::forbidden_message(&required, &granted),
                )),
                deprecation: None,
                status: Some(StatusCode::FORBIDDEN),
                auth_challenge: None,
            }),
            // `authorize_mcp` is the only producer of this variant and
            // is invoked from harn-vm, not from this A2A adapter.
            // Surfacing it here would imply a wiring bug; treat as 403.
            AuthorizationDecision::McpNotAllowlisted { reason, .. } => Err(ProcessedRpc {
                outcome: RpcOutcome::Json(error_response(rpc_id, -32003, &reason)),
                deprecation: None,
                status: Some(StatusCode::FORBIDDEN),
                auth_challenge: None,
            }),
        }
    }
}

pub(super) async fn agent_card_request(State(state): State<HttpState>) -> Response {
    Json(state.server.agent_card(&state.public_url)).into_response()
}

pub(super) async fn jsonrpc_request(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    log_legacy_version_header(&headers);
    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_response(
                    JsonValue::Null,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
            )
                .into_response()
        }
    };
    let auth = http_auth_request(method, "/", body.to_vec(), &headers);
    let processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    rpc_response(processed)
}

// ---- Legacy REST aliases (non-canonical; emit deprecation warnings).
//
// These paths predate the canonical 0.3.0 HTTP+JSON binding under
// `/v1`; each one emits a `Deprecation: true` header plus a
// `Warning: 299 ...` advisory pointing at the canonical replacement.

pub(super) async fn rest_legacy_message_send(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "message/send",
        Some(REST_DEPRECATED_MESSAGE_SEND),
    )
    .await
}

pub(super) async fn rest_legacy_message_stream(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "message/stream",
        Some(REST_DEPRECATED_MESSAGE_STREAM),
    )
    .await
}

pub(super) async fn rest_legacy_send_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/send",
        Some(REST_DEPRECATED_SEND),
    )
    .await
}

pub(super) async fn rest_legacy_send_and_wait_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/send_and_wait",
        Some(REST_DEPRECATED_SEND),
    )
    .await
}

pub(super) async fn rest_legacy_cancel_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/cancel",
        Some(REST_DEPRECATED_CANCEL),
    )
    .await
}

pub(super) async fn rest_legacy_resubscribe_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/resubscribe",
        Some(REST_DEPRECATED_RESUBSCRIBE),
    )
    .await
}

// ---- Canonical A2A 0.3.0 HTTP+JSON/REST binding.

pub(super) async fn rest_v1_message_send(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "message/send", None).await
}

pub(super) async fn rest_v1_message_stream(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "message/stream", None).await
}

pub(super) async fn rest_v1_get_task(
    State(state): State<HttpState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if id.contains(':') {
        // matchit shares the path pattern with `POST /v1/tasks/{id}:cancel` etc.; GET on a
        // custom-method segment is not in the spec.
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "use POST for task custom methods",
        )
            .into_response();
    }
    rest_dispatch_no_body(
        state,
        method,
        headers,
        &format!("/v1/tasks/{id}"),
        "tasks/get",
        json!({"id": id}),
    )
    .await
}

/// Handles `POST /v1/tasks/{id}:cancel` and `POST /v1/tasks/{id}:subscribe`.
///
/// matchit/axum cannot match a literal suffix inside the same path
/// segment as a parameter, so we capture the full segment and parse the
/// `:action` suffix here.
pub(super) async fn rest_v1_task_action(
    State(state): State<HttpState>,
    axum::extract::Path(id_action): axum::extract::Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Some((id, action)) = id_action.rsplit_once(':') else {
        return (StatusCode::NOT_FOUND, "unknown task action").into_response();
    };
    if id.is_empty() {
        return (StatusCode::BAD_REQUEST, "task id required").into_response();
    }
    let rpc_method = match action {
        "cancel" => "tasks/cancel",
        "subscribe" => "tasks/resubscribe",
        _ => return (StatusCode::NOT_FOUND, "unknown task action").into_response(),
    };
    rest_dispatch_no_body(
        state,
        method,
        headers,
        &format!("/v1/tasks/{id_action}"),
        rpc_method,
        json!({"id": id}),
    )
    .await
}

pub(super) async fn rest_v1_card(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    rest_dispatch_no_body(
        state,
        method,
        headers,
        "/v1/card",
        "agent/getAuthenticatedExtendedCard",
        json!({}),
    )
    .await
}

pub(super) async fn rest_v1_push_config_set(
    State(state): State<HttpState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let config = if body.is_empty() {
        JsonValue::Object(Default::default())
    } else {
        match serde_json::from_slice::<JsonValue>(body.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(error_response(
                        JsonValue::Null,
                        -32700,
                        &format!("Parse error: {error}"),
                    )),
                )
                    .into_response()
            }
        }
    };
    let auth_path = format!("/v1/tasks/{id}/pushNotificationConfigs");
    let params = json!({
        "taskId": id,
        "pushNotificationConfig": config,
    });
    rest_dispatch_with_body(
        state,
        method,
        headers,
        &auth_path,
        body,
        "tasks/pushNotificationConfig/set",
        params,
    )
    .await
}

pub(super) async fn rest_v1_push_config_list(
    State(state): State<HttpState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    rest_dispatch_no_body(
        state,
        method,
        headers,
        &format!("/v1/tasks/{id}/pushNotificationConfigs"),
        "tasks/pushNotificationConfig/list",
        json!({"id": id}),
    )
    .await
}

pub(super) async fn rest_v1_push_config_get(
    State(state): State<HttpState>,
    axum::extract::Path((id, config_id)): axum::extract::Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    rest_dispatch_no_body(
        state,
        method,
        headers,
        &format!("/v1/tasks/{id}/pushNotificationConfigs/{config_id}"),
        "tasks/pushNotificationConfig/get",
        json!({"id": id, "pushNotificationConfigId": config_id}),
    )
    .await
}

pub(super) async fn rest_v1_push_config_delete(
    State(state): State<HttpState>,
    axum::extract::Path((id, config_id)): axum::extract::Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    rest_dispatch_no_body(
        state,
        method,
        headers,
        &format!("/v1/tasks/{id}/pushNotificationConfigs/{config_id}"),
        "tasks/pushNotificationConfig/delete",
        json!({"id": id, "pushNotificationConfigId": config_id}),
    )
    .await
}

pub(super) async fn rest_dispatch_no_body(
    state: HttpState,
    method: Method,
    headers: HeaderMap,
    auth_path: &str,
    rpc_method: &str,
    params: JsonValue,
) -> Response {
    log_legacy_version_header(&headers);
    let auth = http_auth_request(method, auth_path, Vec::new(), &headers);
    let request = harn_vm::jsonrpc::request(Uuid::now_v7().to_string(), rpc_method, params);
    let processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    rest_response(processed)
}

pub(super) async fn rest_dispatch_with_body(
    state: HttpState,
    method: Method,
    headers: HeaderMap,
    auth_path: &str,
    body: Bytes,
    rpc_method: &str,
    params: JsonValue,
) -> Response {
    log_legacy_version_header(&headers);
    let auth = http_auth_request(method, auth_path, body.to_vec(), &headers);
    let request = harn_vm::jsonrpc::request(Uuid::now_v7().to_string(), rpc_method, params);
    let processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    rest_response(processed)
}

pub(super) fn rest_response(processed: ProcessedRpc) -> Response {
    let auth_challenge = processed.auth_challenge.clone();
    let response = match processed.outcome {
        RpcOutcome::Json(response) if response.get("error").is_some() => {
            let status = processed.status.unwrap_or(StatusCode::BAD_REQUEST);
            response_with_deprecation(
                (status, Json(response)).into_response(),
                processed.deprecation,
            )
        }
        RpcOutcome::Json(response) => {
            response_with_deprecation(Json(response["result"].clone()), processed.deprecation)
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    };
    apply_auth_challenge(response, auth_challenge)
}

pub(super) async fn rest_task_request(
    state: HttpState,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    rpc_method: &str,
    rest_deprecation: Option<&'static str>,
) -> Response {
    log_legacy_version_header(&headers);
    let params = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_response(
                    JsonValue::Null,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
            )
                .into_response()
        }
    };
    let auth_path = format!("/{rpc_method}");
    let auth = http_auth_request(method, &auth_path, body.to_vec(), &headers);
    let request = harn_vm::jsonrpc::request(Uuid::now_v7().to_string(), rpc_method, params);
    let mut processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    // The caller is on a REST path, so a REST-specific advisory is more
    // actionable than the JSON-RPC method-rename advice the dispatcher
    // already attached. Keep the dispatcher's advice only when the
    // transport doesn't have its own.
    processed.deprecation = rest_deprecation.or(processed.deprecation);
    rest_response(processed)
}

pub(super) fn rpc_response(processed: ProcessedRpc) -> Response {
    let auth_challenge = processed.auth_challenge.clone();
    let response = match processed.outcome {
        RpcOutcome::Json(response) => {
            let mut http = response_with_deprecation(Json(response), processed.deprecation);
            if let Some(status) = processed.status {
                *http.status_mut() = status;
            }
            http
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    };
    apply_auth_challenge(response, auth_challenge)
}

pub(super) fn apply_auth_challenge(
    mut response: Response,
    challenge: Option<HeaderValue>,
) -> Response {
    if let Some(value) = challenge {
        response
            .headers_mut()
            .insert(axum::http::header::WWW_AUTHENTICATE, value);
    }
    response
}

pub(super) fn response_with_deprecation(
    response: impl IntoResponse,
    message: Option<&str>,
) -> Response {
    let mut response = response.into_response();
    if let Some(message) = message {
        response
            .headers_mut()
            .insert(A2A_DEPRECATION_HEADER, HeaderValue::from_static("true"));
        if let Ok(value) = HeaderValue::from_str(&format!("299 harn \"{message}\"")) {
            response
                .headers_mut()
                .insert(axum::http::header::WARNING, value);
        }
    }
    response
}

pub(super) fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    Sse::new(sse_events(rx)).keep_alive(KeepAlive::default())
}

pub(super) fn sse_events(
    rx: UnboundedReceiver<JsonValue>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    rx.map(|message| {
        Ok(Event::default()
            .id(Uuid::now_v7().to_string())
            .event("message")
            .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
    })
}

pub(super) fn empty_stream() -> UnboundedReceiver<JsonValue> {
    let (_tx, rx) = unbounded();
    rx
}

/// Convert a [`A2aPrepareError`] into a finished [`ProcessedRpc`]. Used by
/// `message/send` and `message/stream` to short-circuit when scope
/// preflight or other prepare-time validation rejects the request — the
/// error's HTTP status preference is preserved so the REST `/v1` binding
/// returns `403` for scope mismatches instead of the default 400.
fn prepare_error_to_processed(
    error: A2aPrepareError,
    rpc_id: JsonValue,
    deprecation: Option<&'static str>,
) -> ProcessedRpc {
    let status = error.status_code();
    ProcessedRpc {
        outcome: RpcOutcome::Json(error.with_id(rpc_id)),
        deprecation,
        status,
        auth_challenge: None,
    }
}
