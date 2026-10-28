use super::*;
use harn_vm::triggers::event::ExtensionProviderPayload;
use harn_vm::triggers::{ProviderId, ProviderPayload, SignatureStatus, TriggerEvent};

pub(super) async fn dispatch_to_v1(
    result: JsonValue,
) -> (
    Result<
        (
            harn_vm::a2a::ResolvedA2aEndpoint,
            harn_vm::a2a::DispatchOutcome,
        ),
        harn_vm::a2a::A2aClientError,
    >,
    Vec<MockRequest>,
) {
    dispatch_to_version("1.0", result).await
}

async fn dispatch_to_version(
    version: &'static str,
    result: JsonValue,
) -> (
    Result<
        (
            harn_vm::a2a::ResolvedA2aEndpoint,
            harn_vm::a2a::DispatchOutcome,
        ),
        harn_vm::a2a::A2aClientError,
    >,
    Vec<MockRequest>,
) {
    let server = start_mock_server(move |request| {
        if request.path == "GET /.well-known/agent-card.json" {
            if version == "unauthorized" {
                return status_text(StatusCode::UNAUTHORIZED, "unauthorized");
            }
            if version == "forbidden" {
                return status_text(StatusCode::FORBIDDEN, "forbidden");
            }
            if version == "0.3" {
                if request.headers.contains_key("a2a-version") {
                    return status_text(StatusCode::BAD_REQUEST, "unsupported version");
                }
                return ok_json(json!({"name": "fixture", "protocolVersion": "0.3.0",
                    "preferredTransport": "JSONRPC", "url": format!("http://{}/rpc", request.headers["host"])}));
            }
            if request.headers.get("a2a-version").map(String::as_str) != Some("1.0") {
                return status_text(StatusCode::BAD_REQUEST, "version header required");
            }
            return ok_json(json!({"name": "fixture", "supportedInterfaces": [{
                "protocolBinding": "JSONRPC", "protocolVersion": "1.0",
                "url": format!("http://{}/rpc", request.headers["host"]), "tenant": "team-a"
            }]}));
        }
        if request.path == "POST /rpc" {
            let body: JsonValue = serde_json::from_str(&request.body).unwrap();
            return ok_json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}));
        }
        status_text(StatusCode::NOT_FOUND, "not found")
    })
    .await;
    let target = format!("{}/triage", server.base_url.trim_start_matches("http://"));
    let event = TriggerEvent::new(
        ProviderId::from("test"),
        "issue",
        None,
        "event-1",
        None,
        BTreeMap::new(),
        ProviderPayload::Extension(ExtensionProviderPayload {
            provider: "test".to_string(),
            schema_name: "fixture".to_string(),
            raw: json!({"hello": true}),
        }),
        SignatureStatus::Verified,
    );
    let (_cancel_tx, mut cancel_rx) = tokio::sync::broadcast::channel(1);
    let outcome = if version == "legacy-api" {
        harn_vm::a2a::dispatch_trigger_event(
            &target,
            true,
            "binding",
            "key",
            &event,
            &mut cancel_rx,
        )
        .await
        .map(|(endpoint, ack)| (endpoint, ack.into()))
    } else {
        harn_vm::a2a::dispatch_trigger_outcome(
            &target,
            true,
            "binding",
            "key",
            &event,
            &mut cancel_rx,
        )
        .await
    };
    let requests = server.requests.lock().unwrap().clone();
    (outcome, requests)
}

#[tokio::test]
async fn legacy_rust_api_refuses_v1_before_dispatch() {
    let (outcome, requests) = dispatch_to_version("legacy-api", json!({})).await;
    assert!(matches!(
        outcome,
        Err(harn_vm::a2a::A2aClientError::Protocol(_))
    ));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "GET /.well-known/agent-card.json");
}

#[tokio::test]
async fn discovery_does_not_retry_authorization_failures() {
    for version in ["unauthorized", "forbidden"] {
        let (outcome, requests) = dispatch_to_version(version, json!({})).await;
        assert!(matches!(
            outcome,
            Err(harn_vm::a2a::A2aClientError::Denied(_))
        ));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "GET /.well-known/agent-card.json");
        assert_eq!(
            requests[0].headers.get("a2a-version").map(String::as_str),
            Some("1.0")
        );
    }
}

#[tokio::test]
async fn legacy_dispatch_keeps_v03_wire_format() {
    let (outcome, requests) = dispatch_to_version(
        "0.3",
        json!({
            "id": "legacy-task", "status": {"state": "completed"},
            "history": [{"role": "agent", "parts": [{"type": "text", "text": "legacy"}]}]
        }),
    )
    .await;
    assert!(matches!(outcome.unwrap().1,
        harn_vm::a2a::DispatchOutcome::InlineResult { ref task_id, ref result }
        if task_id == "legacy-task" && result == &json!("legacy")));
    let sent = requests
        .iter()
        .find(|request| request.path == "POST /rpc")
        .unwrap();
    assert_eq!(
        sent.headers.get("a2a-version").map(String::as_str),
        Some("0.3.0")
    );
    let body: JsonValue = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body["method"], "message/send");
    assert_eq!(body["params"]["message"]["role"], "user");
    assert_eq!(body["params"]["message"]["parts"][0]["type"], "text");
    assert!(body["params"].get("tenant").is_none());
    let cards: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "GET /.well-known/agent-card.json")
        .collect();
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards[0].headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );
    assert!(!cards[1].headers.contains_key("a2a-version"));
}

#[tokio::test]
async fn v1_dispatch_uses_discovered_version_tenant_and_task_response() {
    let (outcome, requests) = dispatch_to_v1(json!({"task": {
        "id": "task-1", "status": {"state": "TASK_STATE_COMPLETED"},
        "artifacts": [{"artifactId": "a1", "parts": [{"data": {"answer": 42}}]}]
    }}))
    .await;
    let (_endpoint, ack) = outcome.expect("v1 task dispatch");
    let card = requests
        .iter()
        .find(|request| request.path == "GET /.well-known/agent-card.json")
        .unwrap();
    assert_eq!(
        card.headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );
    assert!(
        matches!(ack, harn_vm::a2a::DispatchOutcome::InlineResult { ref task_id, ref result }
        if task_id == "task-1" && result == &json!({"answer": 42}))
    );
    let sent = requests
        .iter()
        .find(|request| request.path == "POST /rpc")
        .expect("dispatch reached server");
    assert_eq!(
        sent.headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );
    let body: JsonValue = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body["method"], "SendMessage");
    assert_eq!(body["params"]["tenant"], "team-a");
    assert_eq!(body["params"]["message"]["role"], "ROLE_USER");
    assert!(body["params"]["message"]["contextId"].is_string());
    assert!(body["params"]["message"]["parts"][0]["type"].is_null());
    assert!(body["params"]["message"]["parts"][0]["text"].is_string());
}
