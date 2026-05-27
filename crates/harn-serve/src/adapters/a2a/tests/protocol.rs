use super::*;
fn assert_current_agent_card_shape(card: &JsonValue, public_url: &str) {
    assert_eq!(card["name"], "server");
    assert_eq!(card["description"], "Harn peer agent");
    assert_eq!(card["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(card["protocolVersion"], A2A_PROTOCOL_VERSION);
    assert_eq!(card["url"], public_url);
    assert_eq!(card["preferredTransport"], "JSONRPC");
    assert!(
        card.get("supportedInterfaces").is_none(),
        "card must not emit legacy supportedInterfaces"
    );
    assert!(
        card.get("interfaces").is_none(),
        "card must not emit legacy interfaces"
    );
    assert_eq!(
        card["additionalInterfaces"],
        json!([
            {"url": public_url, "transport": "JSONRPC"},
            {
                "url": format!("{}{}", public_url.trim_end_matches('/'), A2A_REST_BASE),
                "transport": "HTTP+JSON",
            }
        ])
    );
    assert_eq!(card["securitySchemes"], json!({}));
    assert_eq!(card["security"], json!([]));
    assert_eq!(
        card["defaultInputModes"],
        json!(["application/json", "text/plain", "application/octet-stream"])
    );
    assert_eq!(
        card["defaultOutputModes"],
        json!(["application/json", "text/plain", "application/octet-stream"])
    );
    assert_eq!(card["capabilities"]["streaming"], true);
    assert_eq!(card["capabilities"]["pushNotifications"], true);
    assert!(
        card["capabilities"].get("extendedAgentCard").is_none(),
        "authenticated extended-card support belongs on supportsAuthenticatedExtendedCard"
    );
    // The default test_server configures no auth methods, so the
    // extended-card capability is advertised as unsupported.
    assert_eq!(card["supportsAuthenticatedExtendedCard"], false);
    assert_eq!(card["skills"][0]["id"], "triage");
    assert_eq!(card["skills"][0]["tags"], json!(["harn", "function"]));
    assert_eq!(
        card["skills"][0]["inputModes"],
        json!(["application/json", "text/plain", "application/octet-stream"])
    );
    assert_eq!(
        card["skills"][0]["outputModes"],
        json!(["application/json", "text/plain", "application/octet-stream"])
    );
}

#[tokio::test]
async fn agent_card_advertises_exported_functions() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );

    let card = server.agent_card("http://localhost:8080");

    assert_current_agent_card_shape(&card, "http://localhost:8080");
}

#[tokio::test]
async fn adapter_agent_card_protocol_fixture_matches_checked_in_matrix() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let actual = vec![server.agent_card("http://localhost:8080")];
    crate::protocol_fixture_tests::assert_fixture_documents_match(
        "conformance/protocols/fixtures/a2a/agent_card_adapter.valid.json",
        actual,
    );
}

#[tokio::test]
async fn discovery_paths_serve_current_agent_card_shape() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let public_url = "http://localhost:8080";
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: public_url.to_string(),
    });

    for path in [
        A2A_AGENT_CARD_PATH,
        "/.well-known/agent.json",
        "/.well-known/a2a-agent",
        "/agent/card",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "path: {path}");
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let card: JsonValue = serde_json::from_slice(&bytes).expect("card json");
        assert_current_agent_card_shape(&card, public_url);
    }
}

#[tokio::test]
async fn legacy_jsonrpc_methods_emit_deprecation_header() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "http://localhost:8080".to_string(),
    });
    let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
        "legacy-1",
        "a2a.SendMessage",
        json!({
            "function": "triage",
            "message": {
                "parts": [{"type": "text", "text": "legacy"}]
            }
        }),
    ))
    .expect("request body");

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(A2A_DEPRECATION_HEADER),
        Some(&HeaderValue::from_static("true"))
    );
    assert!(response
        .headers()
        .get(axum::http::header::WARNING)
        .is_some());
}

#[tokio::test]
async fn unknown_a2a_version_header_no_longer_rejects_request() {
    // Per A2A 0.3.0, version negotiation happens through AgentCard
    // discovery; the request header is non-canonical. A request that
    // carries an unknown `a2a-version` value must still dispatch; the
    // adapter records only a soft-deprecation warning.
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "http://localhost:8080".to_string(),
    });
    let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
        "version-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "hello"}]
            }
        }),
    ))
    .expect("request body");

    for header_value in ["1.0", "0.3.0", "9.9.9", "garbage"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(A2A_VERSION_HEADER, header_value)
                    .body(Body::from(body.clone()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "header {header_value} unexpectedly rejected"
        );
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: JsonValue = serde_json::from_slice(&bytes).expect("json body");
        assert!(
            value.get("error").is_none(),
            "header {header_value} produced JSON-RPC error: {value}"
        );
        assert_eq!(
            value["result"]["status"]["state"], "completed",
            "header {header_value} did not dispatch: {value}"
        );
    }
}

#[tokio::test]
async fn canonical_push_notification_config_methods_round_trip() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let send = harn_vm::jsonrpc::request(
        "send-1",
        "message/send",
        json!({
            "function": "triage",
            "configuration": {"returnImmediately": true},
            "message": {
                "parts": [{"type": "text", "text": "pending"}]
            }
        }),
    );
    let processed = server
        .clone()
        .process_rpc(send, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    assert!(processed.deprecation.is_none());
    let task_id = response["result"]["id"]
        .as_str()
        .expect("task id")
        .to_string();

    let set = harn_vm::jsonrpc::request(
        "push-set",
        "tasks/pushNotificationConfig/set",
        json!({
            "id": task_id,
            "pushNotificationConfig": {
                "id": "push-1",
                "url": "https://client.example/a2a/push"
            }
        }),
    );
    let processed = server
        .clone()
        .process_rpc(set, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push set json response");
    };
    assert_eq!(response["result"]["id"], "push-1");
    assert_eq!(response["result"]["taskId"], task_id);

    let get = harn_vm::jsonrpc::request(
        "push-get",
        "tasks/pushNotificationConfig/get",
        json!({"id": task_id, "pushNotificationConfigId": "push-1"}),
    );
    let processed = server
        .clone()
        .process_rpc(get, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push get json response");
    };
    assert_eq!(response["result"]["url"], "https://client.example/a2a/push");

    let list = harn_vm::jsonrpc::request(
        "push-list",
        "tasks/pushNotificationConfig/list",
        json!({"id": task_id}),
    );
    let processed = server
        .clone()
        .process_rpc(list, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push list json response");
    };
    assert_eq!(response["result"].as_array().expect("configs").len(), 1);

    let delete = harn_vm::jsonrpc::request(
        "push-delete",
        "tasks/pushNotificationConfig/delete",
        json!({"id": task_id, "pushNotificationConfigId": "push-1"}),
    );
    let processed = server.process_rpc(delete, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push delete json response");
    };
    assert!(response["result"].is_null());
}

/// End-to-end exercise of the canonical A2A 0.3.0 HTTP+JSON/REST
/// transport. Walks the full task lifecycle (send, get, list-push,
/// cancel, agent card) and asserts that no canonical path emits a
/// deprecation header.
#[tokio::test]
async fn rest_v1_full_task_lifecycle_uses_canonical_paths() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let public_url = "http://localhost:8080";
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: public_url.to_string(),
    });

    // POST /v1/message:send (returnImmediately=true keeps the task
    // pending so the subsequent get/cancel calls have something to
    // observe).
    let send_body = serde_json::to_vec(&json!({
        "function": "triage",
        "configuration": {"returnImmediately": true},
        "message": {
            "parts": [{"type": "text", "text": "hello"}]
        }
    }))
    .expect("send body");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/message:send")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(send_body))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(A2A_DEPRECATION_HEADER).is_none(),
        "canonical /v1 path must not emit a deprecation header"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let task: JsonValue = serde_json::from_slice(&bytes).expect("task json");
    // The body is the bare Task, not a JSON-RPC envelope.
    assert!(task.get("jsonrpc").is_none());
    assert!(task.get("result").is_none());
    let task_id = task["id"].as_str().expect("task id").to_string();

    // GET /v1/tasks/{id}
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/tasks/{task_id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let fetched: JsonValue = serde_json::from_slice(&bytes).expect("task json");
    assert_eq!(fetched["id"], task_id);

    // POST /v1/tasks/{id}/pushNotificationConfigs
    let config_body = serde_json::to_vec(&json!({
        "id": "rest-push-1",
        "url": "https://client.example/a2a/push"
    }))
    .expect("config body");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/tasks/{task_id}/pushNotificationConfigs"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(config_body))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let config: JsonValue = serde_json::from_slice(&bytes).expect("config json");
    assert_eq!(config["id"], "rest-push-1");
    assert_eq!(config["taskId"], task_id);

    // GET /v1/tasks/{id}/pushNotificationConfigs
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/tasks/{task_id}/pushNotificationConfigs"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let configs: JsonValue = serde_json::from_slice(&bytes).expect("config list");
    assert_eq!(configs.as_array().expect("array").len(), 1);

    // GET /v1/tasks/{id}/pushNotificationConfigs/{configId}
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "/v1/tasks/{task_id}/pushNotificationConfigs/rest-push-1"
                ))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    // DELETE /v1/tasks/{id}/pushNotificationConfigs/{configId}
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!(
                    "/v1/tasks/{task_id}/pushNotificationConfigs/rest-push-1"
                ))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    // POST /v1/tasks/{id}:cancel — exercises the AIP-136 custom-method form.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/tasks/{task_id}:cancel"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let cancelled: JsonValue = serde_json::from_slice(&bytes).expect("task json");
    assert_eq!(cancelled["id"], task_id);
    assert_eq!(cancelled["status"]["state"], "cancelled");

    // POST /v1/tasks/{id}:subscribe — empty resubscribe yields an SSE
    // stream; we just validate the content-type since the task is
    // already terminal.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/tasks/{task_id}:subscribe"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected SSE response, got {content_type}"
    );
}

#[tokio::test]
async fn rest_v1_card_unauthenticated_returns_401_with_www_authenticate() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "https://agent.example".to_string(),
    });

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/card")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .is_some(),
        "401 must carry a WWW-Authenticate challenge"
    );
}

#[tokio::test]
async fn rest_v1_unknown_task_action_returns_404() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "http://localhost:8080".to_string(),
    });

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/tasks/abc:explode")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rest_legacy_message_send_emits_deprecation_advisory() {
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "http://localhost:8080".to_string(),
    });
    let send_body = serde_json::to_vec(&json!({
        "function": "triage",
        "configuration": {"returnImmediately": true},
        "message": {
            "parts": [{"type": "text", "text": "legacy"}]
        }
    }))
    .expect("body");

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/message/send")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(send_body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(A2A_DEPRECATION_HEADER),
        Some(&HeaderValue::from_static("true"))
    );
    let warning = response
        .headers()
        .get(axum::http::header::WARNING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        warning.contains("/v1/message:send"),
        "advisory should point at canonical path, got {warning}"
    );
}

#[tokio::test]
async fn push_notification_configs_survive_server_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let send = harn_vm::jsonrpc::request(
        "send-1",
        "message/send",
        json!({
            "function": "triage",
            "configuration": {"returnImmediately": true},
            "message": {
                "parts": [{"type": "text", "text": "pending"}]
            }
        }),
    );
    let processed = server
        .clone()
        .process_rpc(send, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    let task_id = response["result"]["id"].as_str().expect("task id");

    let set = harn_vm::jsonrpc::request(
        "push-set",
        "tasks/pushNotificationConfig/set",
        json!({
            "id": task_id,
            "pushNotificationConfig": {
                "id": "push-persisted",
                "url": "https://client.example/a2a/persisted"
            }
        }),
    );
    let processed = server.process_rpc(set, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push set json response");
    };
    assert_eq!(response["result"]["id"], "push-persisted");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let restarted = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let get = harn_vm::jsonrpc::request(
        "push-get",
        "tasks/pushNotificationConfig/get",
        json!({"id": task_id, "pushNotificationConfigId": "push-persisted"}),
    );
    let processed = restarted
        .clone()
        .process_rpc(get, AuthRequest::default())
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push get json response");
    };
    assert_eq!(
        response["result"]["url"],
        "https://client.example/a2a/persisted"
    );

    let delete = harn_vm::jsonrpc::request(
        "push-delete",
        "tasks/pushNotificationConfig/delete",
        json!({"id": task_id, "pushNotificationConfigId": "push-persisted"}),
    );
    let processed = restarted.process_rpc(delete, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push delete json response");
    };
    assert!(response["result"].is_null());

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let restarted = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let list = harn_vm::jsonrpc::request(
        "push-list",
        "tasks/pushNotificationConfig/list",
        json!({"id": task_id}),
    );
    let processed = restarted.process_rpc(list, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected push list json response");
    };
    assert!(response["result"].as_array().expect("configs").is_empty());
}

pub(super) fn server_with_api_key_policy(
    source: &str,
    api_key: &str,
) -> (tempfile::TempDir, Arc<A2aServer>) {
    use crate::ApiKeyAuthConfig;
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(&script, source).expect("write script");
    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single(api_key))],
    };
    let core = DispatchCore::new(config).expect("core");
    (dir, Arc::new(A2aServer::new(A2aServerConfig::new(core))))
}

fn auth_request_with_bearer(token: &str) -> AuthRequest {
    AuthRequest {
        method: "POST".to_string(),
        path: "/".to_string(),
        body: Vec::new(),
        headers: std::collections::BTreeMap::from([(
            "authorization".to_string(),
            format!("Bearer {token}"),
        )]),
        validated_oauth: None,
    }
}

fn assert_unauthorized_processed(processed: ProcessedRpc) -> JsonValue {
    assert_eq!(processed.status, Some(StatusCode::UNAUTHORIZED));
    let challenge = processed
        .auth_challenge
        .as_ref()
        .expect("auth challenge")
        .to_str()
        .expect("ascii challenge");
    assert!(
        challenge.starts_with("Bearer realm="),
        "challenge missing scheme: {challenge}"
    );
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    assert_eq!(response["error"]["code"], -32000);
    response
}

#[tokio::test]
async fn protected_message_send_requires_auth_before_task_state_is_created() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let request = harn_vm::jsonrpc::request(
        "auth-gate-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "private task"}]
            },
            "configuration": {"blocking": true}
        }),
    );

    let response = assert_unauthorized_processed(
        server
            .clone()
            .process_rpc(request, AuthRequest::default())
            .await,
    );

    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Unauthorized")),
        "got: {response}"
    );
    assert!(
        server.tasks.lock().expect("tasks poisoned").is_empty(),
        "unauthorized send must not leave task history behind"
    );
}

#[tokio::test]
async fn protected_task_management_requires_auth() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let send = harn_vm::jsonrpc::request(
        "auth-gate-2",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "private task"}]
            },
            "configuration": {"blocking": true}
        }),
    );
    let processed = server
        .clone()
        .process_rpc(send, auth_request_with_bearer("secret"))
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    let task_id = response["result"]["id"]
        .as_str()
        .expect("task id")
        .to_string();

    for request in [
        harn_vm::jsonrpc::request("auth-gate-3", "tasks/get", json!({"id": task_id})),
        harn_vm::jsonrpc::request("auth-gate-4", "tasks/list", json!({})),
        harn_vm::jsonrpc::request("auth-gate-5", "tasks/cancel", json!({"id": task_id})),
        harn_vm::jsonrpc::request(
            "auth-gate-6",
            "tasks/pushNotificationConfig/set",
            json!({
                "taskId": task_id,
                "pushNotificationConfig": {"url": "https://callback.example/push"}
            }),
        ),
    ] {
        assert_unauthorized_processed(
            server
                .clone()
                .process_rpc(request, AuthRequest::default())
                .await,
        );
    }

    assert_eq!(server.task_json(&task_id)["status"]["state"], "completed");
    assert!(
        server
            .push_configs(Some(&task_id))
            .expect("push configs")
            .as_array()
            .expect("push config array")
            .is_empty(),
        "unauthorized push config mutation must not persist"
    );
}

#[tokio::test]
async fn protected_rest_task_management_requires_auth() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let send = harn_vm::jsonrpc::request(
        "auth-gate-rest-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "private task"}]
            },
            "configuration": {"blocking": true}
        }),
    );
    let processed = server
        .clone()
        .process_rpc(send, auth_request_with_bearer("secret"))
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    let task_id = response["result"]["id"]
        .as_str()
        .expect("task id")
        .to_string();
    let router = A2aServer::http_router(HttpState {
        server: server.clone(),
        public_url: "https://agent.example".to_string(),
    });

    let requests = vec![
        Request::builder()
            .method(Method::GET)
            .uri(format!("/v1/tasks/{task_id}"))
            .body(Body::empty())
            .expect("get task request"),
        Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/tasks/{task_id}:cancel"))
            .body(Body::empty())
            .expect("cancel task request"),
        Request::builder()
            .method(Method::GET)
            .uri(format!("/v1/tasks/{task_id}/pushNotificationConfigs"))
            .body(Body::empty())
            .expect("list push configs request"),
        Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/tasks/{task_id}/pushNotificationConfigs"))
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"url":"https://callback.example/push"}"#))
            .expect("set push config request"),
    ];

    for request in requests {
        let response = router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            response
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .is_some(),
            "401 must carry a WWW-Authenticate challenge"
        );
    }

    assert_eq!(server.task_json(&task_id)["status"]["state"], "completed");
    assert!(
        server
            .push_configs(Some(&task_id))
            .expect("push configs")
            .as_array()
            .expect("push config array")
            .is_empty(),
        "unauthorized REST push config mutation must not persist"
    );
}

#[tokio::test]
async fn extended_card_unauthenticated_when_no_auth_configured_returns_not_configured() {
    // Per A2A 0.3.0: if the agent does not have an extended card
    // configured (i.e., no auth scheme is wired in), the server
    // MUST return ExtendedAgentCardNotConfiguredError (-32007).
    let (_dir, server) = test_server(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
    );
    let request =
        harn_vm::jsonrpc::request("card-1", "agent/getAuthenticatedExtendedCard", json!({}));

    let processed = server
        .process_rpc_with_public_url(request, AuthRequest::default(), "https://agent.example")
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    assert!(processed.status.is_none());
    assert!(processed.auth_challenge.is_none());
    assert_eq!(
        response["error"]["code"],
        A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED
    );
}

#[tokio::test]
async fn extended_card_without_token_returns_401_with_challenge() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let request =
        harn_vm::jsonrpc::request("card-2", "agent/getAuthenticatedExtendedCard", json!({}));

    let processed = server
        .clone()
        .process_rpc_with_public_url(request, AuthRequest::default(), "https://agent.example")
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    assert_eq!(processed.status, Some(StatusCode::UNAUTHORIZED));
    let challenge = processed
        .auth_challenge
        .as_ref()
        .expect("auth challenge")
        .to_str()
        .expect("ascii challenge");
    assert!(
        challenge.starts_with("Bearer realm="),
        "challenge missing scheme: {challenge}"
    );
    assert_eq!(response["error"]["code"], -32000);
}

#[tokio::test]
async fn extended_card_with_valid_bearer_returns_extended_payload() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let request =
        harn_vm::jsonrpc::request("card-3", "agent/getAuthenticatedExtendedCard", json!({}));

    let processed = server
        .clone()
        .process_rpc_with_public_url(
            request,
            auth_request_with_bearer("secret"),
            "https://agent.example",
        )
        .await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };
    assert!(processed.status.is_none());
    assert!(processed.auth_challenge.is_none());

    let card = &response["result"];
    assert_eq!(card["name"], "server");
    assert_eq!(card["protocolVersion"], A2A_PROTOCOL_VERSION);
    assert_eq!(card["url"], "https://agent.example");
    assert_eq!(card["preferredTransport"], "JSONRPC");
    assert_eq!(card["additionalInterfaces"][0]["transport"], "JSONRPC");
    assert_eq!(
        card["additionalInterfaces"][0]["url"],
        "https://agent.example"
    );
    assert_eq!(card["additionalInterfaces"][1]["transport"], "HTTP+JSON");
    assert_eq!(
        card["additionalInterfaces"][1]["url"],
        "https://agent.example/v1"
    );
    assert!(card.get("supportedInterfaces").is_none());
    assert_eq!(card["metadata"]["extendedAgentCard"], true);
    assert_eq!(card["metadata"]["principal"], "api-key");
    assert_eq!(card["securitySchemes"]["apiKey"]["type"], "apiKey");
    assert_eq!(card["security"][0]["apiKey"], json!([]));
    assert_eq!(card["skills"][0]["id"], "triage");
    assert_eq!(card["skills"][0]["outputSchema"], json!({}));
}

#[tokio::test]
async fn public_card_advertises_extended_support_when_auth_configured() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );

    let card = server.agent_card("https://agent.example");
    assert!(card["capabilities"].get("extendedAgentCard").is_none());
    assert_eq!(card["supportsAuthenticatedExtendedCard"], true);
    assert_eq!(card["securitySchemes"]["apiKey"]["type"], "apiKey");
    assert_eq!(card["security"][0]["apiKey"], json!([]));
}

#[tokio::test]
async fn http_extended_card_unauthenticated_returns_401_with_www_authenticate() {
    // End-to-end: drive the request through the HTTP router and
    // confirm an unauthenticated JSON-RPC call to
    // agent/getAuthenticatedExtendedCard yields HTTP 401 plus a
    // WWW-Authenticate header.
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let public_url = "https://agent.example";
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: public_url.to_string(),
    });
    let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
        "card-http-1",
        "agent/getAuthenticatedExtendedCard",
        json!({}),
    ))
    .expect("request body");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get(axum::http::header::WWW_AUTHENTICATE)
        .expect("WWW-Authenticate header")
        .to_str()
        .expect("ascii challenge");
    assert!(
        challenge.starts_with("Bearer realm="),
        "challenge missing scheme: {challenge}"
    );
}

#[tokio::test]
async fn http_extended_card_authenticated_returns_extended_payload() {
    let (_dir, server) = server_with_api_key_policy(
        r"
pub fn triage(task: string) -> string {
  return task
}
",
        "secret",
    );
    let public_url = "https://agent.example";
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: public_url.to_string(),
    });
    let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
        "card-http-2",
        "agent/getAuthenticatedExtendedCard",
        json!({}),
    ))
    .expect("request body");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, "Bearer secret")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(axum::http::header::WWW_AUTHENTICATE)
        .is_none());
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let envelope: JsonValue = serde_json::from_slice(&bytes).expect("envelope");
    assert_eq!(envelope["result"]["metadata"]["extendedAgentCard"], true);
    assert_eq!(envelope["result"]["metadata"]["principal"], "api-key");
}

/// End-to-end: a `.harn` handler declares `@scopes("personas:read")`,
/// the auth policy maps API keys to granted-scope sets, and a caller
/// presenting only `sessions:read` is refused with HTTP 403 plus the
/// structured `forbidden` body that callers parse to render an
/// actionable prompt.
#[tokio::test]
async fn message_send_rejects_scope_mismatch_with_http_403_and_structured_body() {
    use crate::{ApiKeyAuthConfig, ApiKeyEntry};
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
@scopes("personas:read")
pub fn list_personas(filter: string) -> string {
  return filter
}
"#,
    )
    .expect("write script");
    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: vec![
                ApiKeyEntry::new("admin-key", ["personas:read".to_string()]),
                ApiKeyEntry::new("limited-key", ["sessions:read".to_string()]),
            ],
        })],
    };
    let core = DispatchCore::new(config).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "https://agent.example".to_string(),
    });

    let send_body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "scope-1",
        "method": "tasks/send_and_wait",
        "params": {
            "function": "list_personas",
            "message": {
                "parts": [{"type": "text", "text": "all"}]
            }
        }
    }))
    .expect("body");

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, "Bearer limited-key")
                .body(Body::from(send_body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: JsonValue = serde_json::from_slice(&bytes).expect("body json");
    assert_eq!(body["error"]["code"], -32003);
    assert_eq!(body["error"]["data"]["kind"], "forbidden");
    assert_eq!(
        body["error"]["data"]["required_scopes"],
        json!(["personas:read"])
    );
    assert_eq!(
        body["error"]["data"]["granted_scopes"],
        json!(["sessions:read"])
    );
    assert_eq!(
        body["error"]["data"]["missing_scopes"],
        json!(["personas:read"])
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("personas:read"),
        "message should mention the missing scope: {body}"
    );
}

#[tokio::test]
async fn message_send_accepts_caller_with_sufficient_scopes() {
    use crate::{ApiKeyAuthConfig, ApiKeyEntry};
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
@scopes("personas:read")
pub fn list_personas(filter: string) -> string {
  return filter
}
"#,
    )
    .expect("write script");
    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: vec![ApiKeyEntry::new("admin-key", ["personas:read".to_string()])],
        })],
    };
    let core = DispatchCore::new(config).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let router = A2aServer::http_router(HttpState {
        server,
        public_url: "https://agent.example".to_string(),
    });

    let send_body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "scope-ok",
        "method": "tasks/send_and_wait",
        "params": {
            "function": "list_personas",
            "message": {
                "parts": [{"type": "text", "text": "engineers"}]
            }
        }
    }))
    .expect("body");

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, "Bearer admin-key")
                .body(Body::from(send_body))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: JsonValue = serde_json::from_slice(&bytes).expect("body json");
    assert!(body.get("error").is_none(), "unexpected error: {body}");
}
