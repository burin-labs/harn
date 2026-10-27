use super::*;
use harn_vm::triggers::event::ExtensionProviderPayload;
use harn_vm::triggers::{ProviderId, ProviderPayload, SignatureStatus, TriggerEvent};

pub(super) async fn dispatch_to_v1(
    result: JsonValue,
) -> (
    Result<
        (harn_vm::a2a::ResolvedA2aEndpoint, harn_vm::a2a::DispatchAck),
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
        (harn_vm::a2a::ResolvedA2aEndpoint, harn_vm::a2a::DispatchAck),
        harn_vm::a2a::A2aClientError,
    >,
    Vec<MockRequest>,
) {
    let server = start_mock_server(move |request| {
        if request.path == "GET /.well-known/agent-card.json" {
            if version == "0.3" {
                return ok_json(json!({"name": "fixture", "protocolVersion": "0.3.0",
                    "preferredTransport": "JSONRPC", "url": format!("http://{}/rpc", request.headers["host"])}));
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
    let outcome = harn_vm::a2a::dispatch_trigger_event(
        &target,
        true,
        "binding",
        "key",
        &event,
        &mut cancel_rx,
    )
    .await;
    let requests = server.requests.lock().unwrap().clone();
    (outcome, requests)
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
        harn_vm::a2a::DispatchAck::InlineResult { ref task_id, ref result }
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
}

#[tokio::test]
async fn v1_dispatch_uses_discovered_version_tenant_and_task_response() {
    let (outcome, requests) = dispatch_to_v1(json!({"task": {
        "id": "task-1", "status": {"state": "TASK_STATE_COMPLETED"},
        "artifacts": [{"artifactId": "a1", "parts": [{"data": {"answer": 42}}]}]
    }}))
    .await;
    let (_endpoint, ack) = outcome.expect("v1 task dispatch");
    assert!(
        matches!(ack, harn_vm::a2a::DispatchAck::InlineResult { ref task_id, ref result }
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
