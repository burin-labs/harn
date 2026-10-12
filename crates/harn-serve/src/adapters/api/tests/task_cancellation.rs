use super::*;

use harn_session_store::task::{TaskStatus, TaskUpdate};

async fn cancel(app: &Router, task_id: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/tasks/{task_id}/cancel"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .expect("cancel request"),
        )
        .await
        .expect("cancel response")
}

#[tokio::test]
async fn task_cancel_is_idempotent_and_preserves_other_terminal_outcomes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let state = server.state.clone();
    let store = state.task_store().unwrap();
    let _live_claim = store.claim_task_execution("task-working").unwrap();
    store
        .admit_task(super::task_retry::admission(
            "task-working",
            dir.path(),
            None,
        ))
        .unwrap();
    store
        .admit_task(super::task_retry::admission(
            "task-completed",
            dir.path(),
            None,
        ))
        .unwrap();
    store
        .update_task(
            "task-completed",
            TaskUpdate {
                status: TaskStatus::Completed,
                at: "completed".into(),
                failure: None,
            },
        )
        .unwrap();
    let app = api_router(state.clone());

    let first = cancel(&app, "task-working").await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = read_json(first).await;
    let canceled_at = first["canceled_at"]
        .as_str()
        .expect("first cancellation timestamp")
        .to_string();

    let second = cancel(&app, "task-working").await;
    assert_eq!(second.status(), StatusCode::OK);
    let second = read_json(second).await;
    assert_eq!(second["canceled_at"], canceled_at);
    assert_eq!(
        second, first,
        "an idempotent retry returns the same resource"
    );

    let completed_before = store.get_task("task-completed").unwrap();
    let terminal = cancel(&app, "task-completed").await;
    assert_eq!(terminal.status(), StatusCode::CONFLICT);
    let terminal_error = read_json(terminal).await;
    assert_eq!(terminal_error["error"]["code"], "task_not_cancelable");

    assert_eq!(store.get_task("task-completed").unwrap(), completed_before);
    assert_eq!(
        store.get_task("task-working").unwrap().resource()["canceled_at"],
        canceled_at
    );
    assert_eq!(
        store
            .task_events(None, None)
            .unwrap()
            .iter()
            .filter(|event| event.event == "task.canceled")
            .count(),
        1,
        "only the state transition emits a cancellation event"
    );
}

#[tokio::test]
async fn task_cancellation_during_cold_restore_does_not_dispatch_a_prompt() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    write_test_pipeline(&script);
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    let mut state = ApiServer::new(config).state;
    let sessions = state.canonical_sessions.clone().unwrap();
    let store = sessions.task_store().unwrap();
    // Keep only the durable descriptor, with no live ACP session.
    store
        .admit_task(super::task_retry::admission("previous", root.path(), None))
        .unwrap();
    store
        .update_task(
            "previous",
            TaskUpdate {
                status: TaskStatus::Completed,
                at: "completed".into(),
                failure: None,
            },
        )
        .unwrap();
    let acp = state.acp.clone();
    let (tx, mut requests) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    let app = api_router(state.clone());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/tasks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"session_id":"session-persisted", "input":"must never start"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    let load = requests.recv().await.unwrap();
    assert_eq!(
        load["method"], "session/load",
        "cold restore must be reached"
    );
    let response = cancel(&app, &task_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["status"], "CANCELED");
    let cancel = requests.recv().await.unwrap();
    assert_eq!(cancel["method"], "session/cancel");
    // Only the dispatch continuation retains this request sender now. Its
    // exit closes the stream, so absence of a prompt is a reached outcome.
    drop(app);
    drop(state);
    acp.resolve_response(json!({"id":load["id"],"result":{}}))
        .await;
    while let Some(request) = requests.recv().await {
        assert_ne!(
            request["method"], "session/prompt",
            "cancellation must survive cold restore acknowledgement"
        );
        acp.resolve_response(json!({"id":request["id"],"result":{}}))
            .await;
    }
    assert_eq!(
        store.get_task(&task_id).unwrap().status,
        TaskStatus::Canceled
    );
}
