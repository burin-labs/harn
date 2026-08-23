use super::*;

fn task(task_id: &str, session_id: &str, status: &str) -> Value {
    json!({
        "id": task_id,
        "object": "task",
        "session_id": session_id,
        "status": status,
        "updated_at": "2026-08-22T00:00:00Z",
        "completed_at": (status == "COMPLETED").then_some("2026-08-22T00:00:00Z"),
        "canceled_at": null,
        "outcome_id": (status == "COMPLETED").then_some(format!("outcome_{task_id}")),
        "failure": (status == "FAILED").then_some(json!({"code": "failed"})),
    })
}

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
    let session_id = "session-cancel-regression";
    {
        let mut inner = state.inner.lock().expect("api state");
        inner.tasks.insert(
            "task-working".to_string(),
            task("task-working", session_id, "WORKING"),
        );
        inner.tasks.insert(
            "task-completed".to_string(),
            task("task-completed", session_id, "COMPLETED"),
        );
        inner
            .active_task_by_session
            .insert(session_id.to_string(), "task-working".to_string());
    }
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

    let completed_before =
        { state.inner.lock().expect("api state").tasks["task-completed"].clone() };
    let terminal = cancel(&app, "task-completed").await;
    assert_eq!(terminal.status(), StatusCode::CONFLICT);
    let terminal_error = read_json(terminal).await;
    assert_eq!(terminal_error["error"]["code"], "task_not_cancelable");

    let inner = state.inner.lock().expect("api state");
    assert_eq!(inner.tasks["task-completed"], completed_before);
    assert_eq!(inner.tasks["task-working"]["canceled_at"], canceled_at);
    assert_eq!(
        inner
            .events
            .iter()
            .filter(|event| event.event == "task.canceled")
            .count(),
        1,
        "only the state transition emits a cancellation event"
    );
}
