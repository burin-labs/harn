use super::*;

#[tokio::test]
async fn advertised_live_control_paths_are_mounted() {
    let app = build_test_router().await;
    let openapi: serde_json::Value =
        harn_vm::yaml::from_str(OPENAPI_YAML).expect("canonical OpenAPI YAML");
    for path in [
        "/v1/sessions/{session_id}/live-clients",
        "/v1/sessions/{session_id}/attach",
        "/v1/sessions/{session_id}/takeover",
        "/v1/sessions/{session_id}/detach",
        "/v1/sessions/{session_id}/heartbeat",
        "/v1/tasks/{task_id}/messages",
    ] {
        assert!(
            openapi["paths"][path].is_object(),
            "canonical OpenAPI must advertise {path}"
        );
        let concrete_path = path
            .replace("{session_id}", "route-probe")
            .replace("{task_id}", "route-probe");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("TRACE")
                    .uri(concrete_path)
                    .body(Body::empty())
                    .expect("route probe"),
            )
            .await
            .expect("route response");
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "OpenAPI path {path} is not mounted in the Axum router"
        );
    }
}

#[tokio::test]
async fn local_api_creates_session_and_accepts_task() {
    let (_dir, server) = pipeline_server(TEST_PIPELINE);
    let state = server.state.clone();
    let mut events = state.events_tx.subscribe();
    let app = api_router(server.state);

    let response = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    let session_id = session["id"].as_str().expect("session id");

    let response = post(&app, &format!("/v1/sessions/{session_id}/tasks"), json!({"input":{"role":"user","parts":[{"type":"text","text":"hello","visibility":"public"}]}})).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task = read_json(response).await;
    assert_eq!(task["object"], "task");
    assert_eq!(task["status"], "SUBMITTED");
    assert!(task["started_at"].is_null());
    assert_eq!(task["session_id"], session_id);
    let task_id = task["id"].as_str().unwrap();
    loop {
        let event = events.recv().await.unwrap();
        if event.task_id.as_deref() == Some(task_id) && event.event == "task.completed" {
            break;
        }
        assert_ne!(event.event, "task.failed", "{}", event.payload);
    }
    let lifecycle: Vec<_> = state
        .task_store()
        .unwrap()
        .task_events(Some(task_id), None)
        .unwrap()
        .into_iter()
        .filter(|event| event.event.starts_with("task."))
        .collect();
    assert_eq!(
        lifecycle
            .iter()
            .map(|event| event.event.as_str())
            .collect::<Vec<_>>(),
        ["task.submitted", "task.started", "task.completed"]
    );
    assert_eq!(lifecycle[0].payload["status"], "SUBMITTED");
    assert!(lifecycle[0].payload["started_at"].is_null());
    assert_eq!(lifecycle[1].payload["status"], "WORKING");
    assert!(lifecycle[1].payload["started_at"].is_string());
}

#[tokio::test]
async fn local_api_projects_vm_owned_live_client_lifecycle() {
    let app = build_test_router().await;
    let response = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    let session_id = session["id"].as_str().expect("session id");

    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/attach"),
        json!({"client_id":"desktop","mode":"controller","metadata":{"surface":"desktop"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let desktop = read_json(response).await;
    assert_eq!(desktop["active_controller_id"], "desktop");
    assert_eq!(desktop["client"]["prompt_injection"], true);
    assert_eq!(desktop["client"]["permission_routing"], true);

    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/attach"),
        json!({"client_id":"mobile","mode":"observer","metadata":{"surface":"mobile-web"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mobile = read_json(response).await;
    assert_eq!(mobile["active_controller_id"], "desktop");
    assert_eq!(mobile["client"]["prompt_injection"], false);
    assert_eq!(mobile["client"]["permission_routing"], false);

    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/takeover"),
        json!({"client_id":"mobile"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let takeover = read_json(response).await;
    assert_eq!(takeover["previous_controller_id"], "desktop");
    assert_eq!(takeover["active_controller_id"], "mobile");
    assert_eq!(takeover["clients"][0]["mode"], "observer");
    assert_eq!(takeover["clients"][1]["mode"], "controller");

    let response = get(&app, &format!("/v1/sessions/{session_id}/live-clients")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let clients = read_json(response).await;
    assert_eq!(clients["object"], "list");
    assert_eq!(clients["data"].as_array().expect("clients").len(), 2);

    let response = get(&app, &format!("/v1/sessions/{session_id}/events")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let events = read_json(response).await;
    let live_updates = events["data"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| event["payload"]["update"]["sessionUpdate"] == "live_session_client")
        .count();
    assert_eq!(live_updates, 3);
}
