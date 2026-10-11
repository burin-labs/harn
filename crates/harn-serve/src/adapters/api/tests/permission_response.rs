use super::*;
use harn_session_store::task::TaskStatus;

fn pending_permission(root: &Path, hitl: bool) -> (ApiState, mpsc::UnboundedReceiver<Value>) {
    let script = root.join("agent.harn");
    write_test_pipeline(&script);
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.into();
    let mut state = ApiServer::new(config).state;
    state
        .task_store()
        .unwrap()
        .admit_task(super::task_retry::admission("waiting", root, None))
        .unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    if hitl {
        state.register_hitl_request(Some("session-persisted".into()), Some("waiting".into()), &json!({
            "update":{"_meta":{"harn":{"requestId":"permission-one", "kind":"approve", "payload":{}}}},
        }));
    } else {
        state.register_permission_request(json!({
            "id":41,"method":"session/request_permission",
            "params":{"sessionId":"session-persisted","toolCall":{"toolCallId":"permission-one"}},
        }));
    }
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::InputRequired
    );
    (state, rx)
}

async fn respond(app: &Router, approved: bool) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/permission-requests/permission-one/respond")
                .header("content-type", "application/json")
                .body(Body::from(json!({"approved":approved}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn permission(app: &Router) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/permission-requests")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    body["data"][0].clone()
}

#[tokio::test]
async fn permission_response_keeps_pending_on_store_failure_then_delivers_once() {
    let root = tempfile::tempdir().unwrap();
    let (mut state, mut requests) = pending_permission(root.path(), false);
    let before = permission(&api_router(state.clone())).await;
    let store = state.canonical_sessions.take();
    let unavailable = api_router(state.clone());
    let response = respond(&unavailable, true).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        read_json(response).await["error"]["code"],
        "task_store_unavailable"
    );
    assert_eq!(permission(&unavailable).await, before);
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    state.canonical_sessions = store;
    let app = api_router(state.clone());
    let response = respond(&app, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(permission(&app).await["status"], "approved");
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::Working
    );
    let delivered = requests
        .try_recv()
        .expect("the success control reaches ACP");
    assert_eq!(delivered["id"], 41);
    assert_eq!(delivered["result"]["outcome"]["optionId"], "allow");
    assert_eq!(respond(&app, false).await.status(), StatusCode::CONFLICT);
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn permission_response_keeps_pending_on_failed_hitl_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut requests) = pending_permission(root.path(), true);
    let app = api_router(state.clone());
    let before = permission(&app).await;
    let answer = respond(&app, true);
    let rejection = async {
        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "harn.hitl.respond");
        state
            .acp
            .resolve_response(json!({"id":request["id"],"error":{"message":"not accepted"}}))
            .await;
    };
    let (response, ()) = tokio::join!(answer, rejection);
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(permission(&app).await, before);
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::InputRequired
    );
    let accepted = async {
        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "harn.hitl.respond");
        state
            .acp
            .resolve_response(json!({"id":request["id"],"result":{}}))
            .await;
    };
    let (response, ()) = tokio::join!(respond(&app, true), accepted);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(permission(&app).await["status"], "approved");
}

#[tokio::test]
async fn permission_response_claim_excludes_concurrent_answers_and_releases_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut requests) = pending_permission(root.path(), true);
    let app = api_router(state.clone());
    let before = permission(&app).await;
    let mut answer = Box::pin(respond(&app, true));
    let request = tokio::select! {
        request = requests.recv() => request.unwrap(),
        response = &mut answer => panic!("response completed without ACP acknowledgement: {:?}", response.status()),
    };
    assert_eq!(request["method"], "harn.hitl.respond");
    assert_eq!(respond(&app, false).await.status(), StatusCode::CONFLICT);
    assert_eq!(permission(&app).await, before);
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(answer);
    assert_eq!(permission(&app).await, before);
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::InputRequired
    );
    state
        .acp
        .resolve_response(json!({"id":request["id"],"error":{"message":"not accepted"}}))
        .await;
    let accepted = async {
        let request = requests.recv().await.unwrap();
        state
            .acp
            .resolve_response(json!({"id":request["id"],"result":{}}))
            .await;
    };
    let (response, ()) = tokio::join!(respond(&app, true), accepted);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(permission(&app).await["status"], "approved");
}

#[tokio::test]
async fn permission_response_keeps_pending_when_acp_channel_is_closed() {
    let root = tempfile::tempdir().unwrap();
    let (state, requests) = pending_permission(root.path(), false);
    let app = api_router(state.clone());
    let before = permission(&app).await;
    drop(requests);
    assert_eq!(respond(&app, true).await.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(permission(&app).await, before);
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::InputRequired
    );
}
