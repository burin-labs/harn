use super::*;

fn assert_event_envelope(event: &Value) {
    let specification: Value = harn_vm::yaml::from_str(OPENAPI_YAML).unwrap();
    for schema in [
        &specification["components"]["schemas"]["ResourceEnvelope"],
        &specification["components"]["schemas"]["Event"]["allOf"][1],
    ] {
        for field in schema["required"].as_array().unwrap() {
            assert!(
                event.get(field.as_str().unwrap()).is_some(),
                "missing {field}: {event}"
            );
        }
    }
    assert_eq!(event["object"], "event");
    assert_eq!(event["updated_at"], event["created_at"]);
    assert_eq!(event["metadata"], json!({}));
    let sequence = event_seq(event["id"].as_str().unwrap());
    assert!(sequence > 0, "must reach a durably allocated event");
    assert_eq!(event["sequence"], sequence);
    assert!(!event["resource"]["object"].as_str().unwrap().is_empty());
    assert!(!event["resource"]["id"].as_str().unwrap().is_empty());
}

async fn first_sse_event(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let frame = stream.next().await.unwrap().unwrap();
    let frame = std::str::from_utf8(&frame).unwrap();
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data:"))
        .unwrap();
    serde_json::from_str(data.trim()).unwrap()
}

#[tokio::test]
async fn event_envelope_matches_live_replay_and_restarted_task_history() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_preparation.harn")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_default_session_mode("code")
        .unwrap();
    config.workspace_root = root.path().into();
    let server = ApiServer::new(config.clone());
    let state = server.state.clone();
    let app = api_router(state.clone());
    let live = get(&app, "/v1/events/stream").await;
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let session_event = first_sse_event(live).await;
    assert_event_envelope(&session_event);
    assert_eq!(
        session_event["resource"],
        json!({"object":"session", "id":session_id})
    );

    let message = post(
        &app,
        &format!("/v1/sessions/{session_id}/messages"),
        json!({"message":{"text":"known message"}}),
    )
    .await;
    assert_eq!(message.status(), StatusCode::CREATED);
    let message_id = read_json(message).await["id"].as_str().unwrap().to_owned();
    let history = read_json(get(&app, &format!("/v1/sessions/{session_id}/events")).await).await;
    let message_event = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event"] == "message.created")
        .unwrap();
    assert_event_envelope(message_event);
    assert_eq!(
        message_event["resource"],
        json!({"object":"message", "id":message_id})
    );

    let mut updates = state.events_tx.subscribe();
    let task = post(
        &app,
        &format!("/v1/sessions/{session_id}/tasks"),
        json!({"input":"execute once"}),
    )
    .await;
    assert_eq!(task.status(), StatusCode::ACCEPTED);
    let task_id = read_json(task).await["id"].as_str().unwrap().to_owned();
    loop {
        let event = updates.recv().await.unwrap();
        if event.task_id.as_deref() == Some(task_id.as_str()) && event.event == "task.completed" {
            break;
        }
        assert_ne!(event.event, "task.failed", "{}", event.payload);
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("task-preparation.txt")).unwrap(),
        "vm-executed"
    );
    let path = format!("/v1/tasks/{task_id}/events");
    let before = read_json(get(&app, &path).await).await;
    let durable = state
        .task_store()
        .unwrap()
        .task_events(Some(&task_id), None)
        .unwrap();
    assert!(!durable.is_empty());
    for event in before["data"].as_array().unwrap() {
        assert_event_envelope(event);
    }
    let submitted = before["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event"] == "task.submitted")
        .unwrap();
    assert_eq!(
        submitted["resource"],
        json!({"object":"task", "id":task_id})
    );
    let replay = first_sse_event(get(&app, &format!("/v1/tasks/{task_id}/stream")).await).await;
    assert_event_envelope(&replay);
    assert_eq!(replay, before["data"][0]);

    let restarted = api_router(ApiServer::new(config).state);
    let after = read_json(get(&restarted, &path).await).await;
    let session_history =
        read_json(get(&restarted, &format!("/v1/sessions/{session_id}/events")).await).await;
    assert!(
        !session_history["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["id"] == session_event["id"]),
        "the known non-task event is process-local"
    );
    assert!(
        session_history["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["id"] == submitted["id"]),
        "the known task control event is durable"
    );
    for stored in durable {
        let old = before["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["id"] == stored.id)
            .unwrap();
        let new = after["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["id"] == stored.id)
            .unwrap();
        assert_event_envelope(new);
        assert_eq!(
            new, old,
            "replay must retain allocation identity and resource"
        );
    }
}

#[tokio::test]
async fn workspace_registration_resolves_and_confines_known_files() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_preparation.harn")).unwrap();
    std::fs::write(root.path().join("canary.txt"), "inside").unwrap();
    std::fs::write(outside.path().join("canary.txt"), "outside").unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/canary.txt"), "nested").unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    let app = api_router(ApiServer::new(config).state);
    for (input, expected_root, content) in [
        (json!({}), root.path().to_path_buf(), "inside"),
        (json!({"root":"."}), root.path().to_path_buf(), "inside"),
        (
            json!({"root":root.path()}),
            root.path().to_path_buf(),
            "inside",
        ),
        (
            json!({"root":"nested"}),
            root.path().join("nested"),
            "nested",
        ),
    ] {
        let response = post(&app, "/v1/workspaces", input).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let workspace = read_json(response).await;
        assert_eq!(
            workspace["root"],
            expected_root
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        let file = get(
            &app,
            &format!(
                "/v1/workspaces/{}/files?path=canary.txt",
                workspace["id"].as_str().unwrap()
            ),
        )
        .await;
        assert_eq!(file.status(), StatusCode::OK);
        assert_eq!(read_json(file).await["content"], content);
        let written = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/v1/workspaces/{}/files",
                        workspace["id"].as_str().unwrap()
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"path":"written.txt","content":"inside-write"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(written.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(expected_root.join("written.txt")).unwrap(),
            "inside-write"
        );
    }
    let mut invalid = vec![
        json!(42),
        json!(null),
        json!(""),
        json!("missing"),
        json!("canary.txt"),
        json!("nested/../nested"),
    ];
    invalid.push(json!(outside.path()));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        invalid.push(json!("escape"));
    }
    let before = read_json(get(&app, "/v1/workspaces").await).await["data"]
        .as_array()
        .unwrap()
        .len();
    for candidate in invalid {
        let response = post(&app, "/v1/workspaces", json!({"root":candidate})).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "root={candidate}"
        );
        assert_eq!(
            read_json(response).await["error"]["code"],
            "invalid_workspace_root"
        );
    }
    assert_eq!(
        read_json(get(&app, "/v1/workspaces").await).await["data"]
            .as_array()
            .unwrap()
            .len(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("canary.txt")).unwrap(),
        "outside"
    );
}

#[tokio::test]
async fn session_workspace_is_resolved_before_any_creation_effect() {
    fn identities(list: &Value, event: Option<&str>) -> BTreeSet<String> {
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| event.is_none_or(|event| item["event"] == event))
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    }
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_preparation.harn")).unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let root_id = state.inner.lock().unwrap().root_workspace_id.clone();
    let default = post(&app, "/v1/sessions", json!({})).await;
    assert_eq!(default.status(), StatusCode::CREATED);
    assert_eq!(read_json(default).await["workspace_id"], root_id);
    let workspace = post(&app, "/v1/workspaces", json!({"root":"nested"})).await;
    assert_eq!(workspace.status(), StatusCode::CREATED);
    let workspace_id = read_json(workspace).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = post(
        &app,
        "/v1/sessions",
        json!({"workspace_id":workspace_id,"mode_id":"code"}),
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session = read_json(session).await;
    assert_eq!(session["workspace_id"], workspace_id);
    let session_id = session["id"].as_str().unwrap().to_owned();
    let before_sessions = read_json(get(&app, "/v1/sessions").await).await;
    let before_events = read_json(get(&app, "/v1/events").await).await;
    let session_ids = identities(&before_sessions, None);
    let creation_ids = identities(&before_events, Some("session.created"));
    assert_eq!(session_ids.len(), 2);
    assert_eq!(creation_ids.len(), 2);
    for (id, status, code) in [
        (
            json!("workspace_missing_known_canary"),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (json!(42), StatusCode::BAD_REQUEST, "invalid_workspace"),
        (json!(null), StatusCode::BAD_REQUEST, "invalid_workspace"),
        (json!(" "), StatusCode::BAD_REQUEST, "invalid_workspace"),
    ] {
        let response = post(&app, "/v1/sessions", json!({"workspace_id":id})).await;
        assert_eq!(response.status(), status);
        assert_eq!(read_json(response).await["error"]["code"], code);
        assert_eq!(
            identities(&read_json(get(&app, "/v1/sessions").await).await, None),
            session_ids
        );
        assert_eq!(
            identities(
                &read_json(get(&app, "/v1/events").await).await,
                Some("session.created")
            ),
            creation_ids
        );
    }
    let mut events = state.events_tx.subscribe();
    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/tasks"),
        json!({"input":"registered workspace execution"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    loop {
        let event = events.recv().await.unwrap();
        if event.task_id.as_deref() == Some(task_id.as_str()) {
            assert_ne!(event.event, "task.failed", "{}", event.payload);
            if event.event == "task.completed" {
                break;
            }
        }
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/task-preparation.txt")).unwrap(),
        "vm-executed"
    );
    assert!(!root.path().join("task-preparation.txt").exists());
}
