use super::*;
use crate::AcpJsonRpcError;
use std::sync::atomic::AtomicUsize;

struct FixtureHost {
    calls: AtomicUsize,
}

impl ApiHostResponder for FixtureHost {
    fn respond(&self, method: &str, params: &Value) -> Result<Value, AcpJsonRpcError> {
        match method {
            "host/capabilities" => Ok(json!({"project": ["metadata_get"]})),
            "host/call" if params["name"] == "project.metadata_get" => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                assert!(params["sessionId"].is_string());
                assert_eq!(params["args"]["dir"], "proof");
                Ok(json!({"proof": {"owner": "embedder", "observed": true}}))
            }
            _ => Err(AcpJsonRpcError {
                code: -32001,
                message: "Fixture operation refused".into(),
                data: Some(json!({"reason": "not_granted"})),
            }),
        }
    }
}

struct Binder(Arc<FixtureHost>);

impl ApiHostResponderBinder for Binder {
    fn bind(&self, _root: &std::path::Path) -> Result<Arc<dyn ApiHostResponder>, String> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn submitted_task_reaches_embedder_host_and_missing_responder_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    std::fs::write(
        &script,
        r#"pipeline main(harness: Harness) {
  const facts = harness.project.metadata_get({dir: "proof"})
  assert(facts.proof.owner == "embedder" && facts.proof.observed, "the concrete host call must fire")
  harness.stdio.println("embedder-host-observed")
}
"#,
    )
    .unwrap();
    let responder = Arc::new(FixtureHost {
        calls: AtomicUsize::new(0),
    });
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_host_responder(Arc::new(Binder(responder.clone())));
    config.workspace_root = dir.path().into();
    let server = ApiServer::new(config);
    let mut events = server.state.events_tx.subscribe();
    let app = api_router(server.state);
    let session = super::served_agent_turn::create_session(&app).await;
    let terminal =
        super::served_agent_turn::submit_and_wait(&app, &mut events, &session, "read").await;
    assert_eq!(terminal.event, "task.completed", "{}", terminal.payload);
    assert_eq!(responder.calls.load(Ordering::SeqCst), 1);

    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = dir.path().into();
    let server = ApiServer::new(config);
    let mut events = server.state.events_tx.subscribe();
    let app = api_router(server.state);
    let session = super::served_agent_turn::create_session(&app).await;
    let terminal =
        super::served_agent_turn::submit_and_wait(&app, &mut events, &session, "read").await;
    assert_eq!(terminal.event, "task.failed", "{}", terminal.payload);
    assert_eq!(
        responder.calls.load(Ordering::SeqCst),
        1,
        "an absent hook must never reach another server's host"
    );
}

#[tokio::test]
async fn responder_does_not_intercept_permissions() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    write_test_pipeline(&script);
    let responder = Arc::new(FixtureHost {
        calls: AtomicUsize::new(0),
    });
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_host_responder(Arc::new(Binder(responder.clone())));
    config.workspace_root = root.path().into();
    let mut state = ApiServer::new(config).state;
    let (tx, mut rx) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    state
        .handle_acp_request(json!({"id":8,"method":"session/request_permission",
        "params":{"sessionId":"session-one","toolCall":{"toolCallId":"approval-one"}}}))
        .await;
    assert_eq!(responder.calls.load(Ordering::SeqCst), 0);
    assert!(state
        .inner
        .lock()
        .unwrap()
        .permissions
        .contains_key("approval-one"));
    assert!(
        rx.try_recv().is_err(),
        "permissions must wait for the API decision"
    );
    let app = api_router(state);
    let visible = get(&app, "/v1/permission-requests").await;
    assert_eq!(visible.status(), StatusCode::OK);
    let visible = read_json(visible).await;
    assert_eq!(visible["data"].as_array().unwrap().len(), 1);
    assert_eq!(visible["data"][0]["id"], "approval-one");
    assert_eq!(visible["data"][0]["status"], "pending");
    let denied = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/permission-requests/approval-one/respond")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"approved":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(read_json(denied).await["status"], "denied");
    let reply = harn_clock::test_support::within("permission rejection response", rx.recv())
        .await
        .unwrap();
    assert_eq!(reply["id"], 8);
    assert_eq!(reply["result"]["outcome"]["optionId"], "reject");
    assert!(
        reply.get("error").is_none(),
        "a visible rejection is an ACP outcome"
    );
}
