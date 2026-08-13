use super::*;

#[tokio::test]
async fn advertised_live_control_paths_are_mounted() {
    let app = build_test_router().await;
    let openapi: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(OPENAPI_YAML).expect("canonical OpenAPI YAML");
    for path in [
        "/v1/sessions/{session_id}/live-clients",
        "/v1/sessions/{session_id}/attach",
        "/v1/sessions/{session_id}/takeover",
        "/v1/sessions/{session_id}/detach",
        "/v1/sessions/{session_id}/heartbeat",
        "/v1/tasks/{task_id}/messages",
    ] {
        assert!(
            openapi["paths"][path].is_mapping(),
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
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let server = ApiServer::new(ApiServerConfig::for_pipeline(
        script.to_string_lossy().to_string(),
    ));
    let app = api_router(server.state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let session: Value = serde_json::from_slice(&body).expect("session");
    let session_id = session["id"].as_str().expect("session id");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/tasks"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"input":{"role":"user","parts":[{"type":"text","text":"hello","visibility":"public"}]}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("task response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let task: Value = serde_json::from_slice(&body).expect("task");
    assert_eq!(task["object"], "task");
    assert_eq!(task["status"], "WORKING");
    assert_eq!(task["session_id"], session_id);
}

#[tokio::test]
async fn local_api_projects_vm_owned_live_client_lifecycle() {
    let app = build_test_router().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"workspace_id":"local"}"#))
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    let session_id = session["id"].as_str().expect("session id");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/attach"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"client_id":"desktop","mode":"controller","metadata":{"surface":"desktop"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("desktop attach");
    assert_eq!(response.status(), StatusCode::OK);
    let desktop = read_json(response).await;
    assert_eq!(desktop["active_controller_id"], "desktop");
    assert_eq!(desktop["client"]["prompt_injection"], true);
    assert_eq!(desktop["client"]["permission_routing"], true);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/attach"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"client_id":"mobile","mode":"observer","metadata":{"surface":"mobile-web"}}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("mobile attach");
    assert_eq!(response.status(), StatusCode::OK);
    let mobile = read_json(response).await;
    assert_eq!(mobile["active_controller_id"], "desktop");
    assert_eq!(mobile["client"]["prompt_injection"], false);
    assert_eq!(mobile["client"]["permission_routing"], false);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session_id}/takeover"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"client_id":"mobile"}"#))
                .expect("request"),
        )
        .await
        .expect("mobile takeover");
    assert_eq!(response.status(), StatusCode::OK);
    let takeover = read_json(response).await;
    assert_eq!(takeover["previous_controller_id"], "desktop");
    assert_eq!(takeover["active_controller_id"], "mobile");
    assert_eq!(takeover["clients"][0]["mode"], "observer");
    assert_eq!(takeover["clients"][1]["mode"], "controller");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session_id}/live-clients"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("live clients");
    assert_eq!(response.status(), StatusCode::OK);
    let clients = read_json(response).await;
    assert_eq!(clients["object"], "list");
    assert_eq!(clients["data"].as_array().expect("clients").len(), 2);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session_id}/events"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("events");
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
