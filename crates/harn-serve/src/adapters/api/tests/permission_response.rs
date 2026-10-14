use super::*;
use harn_session_store::task::TaskStatus;

fn pending_permission(root: &Path, hitl: bool) -> (ApiState, mpsc::UnboundedReceiver<Value>) {
    pending_permission_for(root, hitl, "permission-one")
}

fn pending_permission_for(
    root: &Path,
    hitl: bool,
    request_id: &str,
) -> (ApiState, mpsc::UnboundedReceiver<Value>) {
    pending_permission_kind(root, hitl, request_id, "question")
}

fn pending_permission_kind(
    root: &Path,
    hitl: bool,
    request_id: &str,
    kind: &str,
) -> (ApiState, mpsc::UnboundedReceiver<Value>) {
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
        state.register_hitl_request(
            Some("session-persisted".into()),
            Some("waiting".into()),
            &json!({
                "update":{"_meta":{"harn":{"requestId":request_id, "kind":kind, "payload":{}}}},
            }),
        );
    } else {
        state.register_permission_request(json!({
            "id":41,"method":"session/request_permission",
            "params":{"sessionId":"session-persisted","toolCall":{"toolCallId":request_id},
                "_meta":{"harn":{"task":{"taskId":"waiting"}}}},
        }));
    }
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        if hitl && kind == "question" {
            TaskStatus::InputRequired
        } else {
            TaskStatus::AuthRequired
        }
    );
    (state, rx)
}

#[tokio::test]
async fn failed_hitl_approval_restores_auth_required() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut requests) =
        pending_permission_kind(root.path(), true, "approval-one", "approval");
    let app = api_router(state.clone());
    let answer = respond_to(&app, "approval-one", true);
    let response = async {
        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "harn.hitl.respond");
        state
            .acp
            .resolve_response(
                json!({"id":request["id"],"error":{"code":-32000,"message":"not accepted"}}),
            )
            .await;
    };
    let (answer, ()) = tokio::join!(answer, response);
    assert_eq!(answer.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task("waiting")
            .unwrap()
            .status,
        TaskStatus::AuthRequired
    );
    assert_eq!(permission(&app).await["status"], "pending");
}

async fn respond(app: &Router, approved: bool) -> Response {
    respond_to(app, "permission-one", approved).await
}

async fn respond_to(app: &Router, request_id: &str, approved: bool) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/permission-requests/{request_id}/respond"))
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
async fn permission_response_claim_survives_caller_drop_until_rejected_ack() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut requests) = pending_permission(root.path(), true);
    let app = api_router(state.clone());
    let mut events = state.events_tx.subscribe();
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
        TaskStatus::Working
    );
    assert_eq!(respond(&app, false).await.status(), StatusCode::CONFLICT);
    state
        .acp
        .resolve_response(json!({"id":request["id"],"error":{"message":"not accepted"}}))
        .await;
    loop {
        let event = events.recv().await.unwrap();
        if event.event == "task.input_required" {
            break;
        }
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
async fn permission_response_settles_late_hitl_success_after_caller_drop() {
    use harn_vm::event_log::{EventLog, Topic};

    let root = tempfile::tempdir().unwrap();
    let request_id = "hitl_question_permission_one";
    let (state, mut requests) = pending_permission_for(root.path(), true, request_id);
    let log = harn_vm::event_log::install_memory_for_current_thread(32);
    let chunk = harn_vm::compile_source(
        "fn main(harness: Harness) { harness.runtime.waitpoint_create(\"hitl_question_permission_one\") }",
    )
    .unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.unwrap();
        })
        .await;
    let waitpoint_topic = Topic::new("waitpoint.state.hitl_question_permission_one").unwrap();
    assert_eq!(
        log.read_range(&waitpoint_topic, None, usize::MAX)
            .await
            .unwrap()
            .last()
            .unwrap()
            .1
            .payload["status"],
        "open"
    );
    let app = api_router(state.clone());
    let mut events = state.events_tx.subscribe();
    let mut answer = Box::pin(respond_to(&app, request_id, true));
    let request = tokio::select! {
        request = requests.recv() => request.unwrap(),
        response = &mut answer => panic!("response completed without ACP acknowledgement: {:?}", response.status()),
    };
    assert_eq!(request["method"], "harn.hitl.respond");
    assert_eq!(request["params"]["request_id"], request_id);
    assert_eq!(request["params"]["approved"], true);
    assert_eq!(
        respond_to(&app, request_id, false).await.status(),
        StatusCode::CONFLICT
    );
    drop(answer);

    let event_id = harn_vm::append_hitl_response(
        Some(root.path()),
        serde_json::from_value(request["params"].clone()).unwrap(),
    )
    .await
    .expect("the queued HITL request is applied before its acknowledgement");
    assert_eq!(
        log.read_range(&waitpoint_topic, None, usize::MAX)
            .await
            .unwrap()
            .last()
            .unwrap()
            .1
            .payload["status"],
        "completed"
    );
    state
        .acp
        .resolve_response(json!({"id":request["id"],"result":{"event_id":event_id}}))
        .await;

    let settlement = async {
        loop {
            let event = events.recv().await.unwrap();
            if event.event == "permission.responded" {
                break event;
            }
        }
    };
    let conflicting = async {
        let mut retry = Box::pin(respond_to(&app, request_id, false));
        tokio::select! {
            response = &mut retry => response,
            extra = requests.recv() => {
                // Let an incorrect second delivery finish, so the old owner fails
                // on its conflicting outcome instead of waiting forever for an ACK.
                let extra = extra.unwrap();
                assert_eq!(extra["method"], "harn.hitl.respond");
                assert_eq!(extra["params"]["approved"], false);
                harn_vm::append_hitl_response(Some(root.path()), serde_json::from_value(extra["params"].clone()).unwrap()).await.unwrap();
                state.acp.resolve_response(json!({"id":extra["id"],"result":{}})).await;
                retry.await
            }
        }
    };
    let (settled, opposite) = tokio::join!(settlement, conflicting);
    assert_eq!(settled.payload["status"], "approved");
    assert_eq!(opposite.status(), StatusCode::CONFLICT);
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
    let audit = state.permissions.history(&AuditFilter::default()).await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].request.id, request_id);
    assert_eq!(audit[0].outcome, crate::permissions::AuditOutcome::Granted);
    let persisted = state
        .task_store()
        .unwrap()
        .task_events(Some("waiting"), None)
        .unwrap();
    let responses: Vec<_> = persisted
        .iter()
        .filter(|event| event.event == "permission.responded")
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].payload["status"], "approved");
    let received = log
        .read_range(
            &Topic::new(harn_vm::HITL_QUESTIONS_TOPIC).unwrap(),
            None,
            usize::MAX,
        )
        .await
        .unwrap();
    assert_eq!(
        received
            .iter()
            .filter(|(_, event)| event.kind == "hitl.response_received")
            .count(),
        1
    );
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
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
        TaskStatus::AuthRequired
    );
}
