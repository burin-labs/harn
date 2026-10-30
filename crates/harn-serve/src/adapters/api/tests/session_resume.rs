use super::*;

async fn completed_session(root: &Path) -> (ApiServerConfig, String, String) {
    let script = root.join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_default_session_mode("code")
        .unwrap();
    config.workspace_root = root.into();
    let server = ApiServer::new(config.clone());
    let mut events = server.state.events_tx.subscribe();
    let app = api_router(server.state);
    let response = request(
        &app,
        "POST",
        "/v1/sessions",
        json!({"workspace_id":"local"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await["id"].as_str().unwrap().to_owned();
    let task =
        submit_completed_task(&app, &mut events, &session, "initial-transcript-canary").await;
    assert_eq!(
        std::fs::read_to_string(root.join("task-retry-provider.txt")).unwrap(),
        "provider-reached\n"
    );
    (config, session, task)
}

fn completed_prompt_message_count(root: &Path) -> usize {
    let calls: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("task-retry-provider-request.json")).unwrap(),
    )
    .unwrap();
    let calls = calls.as_array().expect("captured provider calls");
    assert_eq!(
        calls.len(),
        1,
        "the fixture reaches exactly one provider call per task"
    );
    let messages = calls[0]["messages"]
        .as_array()
        .expect("actual provider request messages");
    assert!(
        !messages.is_empty(),
        "the provider must receive real prompt context"
    );
    messages.len() + 1
}

async fn submit_completed_task(
    app: &Router,
    events: &mut tokio::sync::broadcast::Receiver<ApiEvent>,
    session: &str,
    input: &str,
) -> String {
    let response = request(
        app,
        "POST",
        &format!("/v1/sessions/{session}/tasks"),
        json!({"input":input}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task = read_json(response).await["id"].as_str().unwrap().to_owned();
    loop {
        let event = events.recv().await.unwrap();
        if event.task_id.as_deref() == Some(&task) {
            assert_ne!(event.event, "task.failed", "{}", event.payload);
            if event.event == "task.completed" {
                break;
            }
        }
    }
    task
}

#[tokio::test]
async fn accepted_forks_retain_nested_lineage_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let (config, original, _) = completed_session(root.path()).await;
    let mut server = ApiServer::new(config.clone());
    let mut app = api_router(server.state.clone());
    let mut parent = original.clone();
    let response = request(
        &app,
        "PATCH",
        &format!("/v1/sessions/{parent}"),
        json!({"metadata":{"known_parent":true}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let before = {
        let inner = server.state.inner.lock().unwrap();
        assert!(inner.sessions.contains_key(&parent));
        (inner.sessions.len(), inner.events.len())
    };
    for invalid in [json!({"branch_id":7}), json!({"metadata":[]}), json!(null)] {
        let response = request(
            &app,
            "POST",
            &format!("/v1/sessions/{parent}/fork"),
            invalid,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{parent}/fork"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    for invalid in [json!({"metadata":[]}), json!({"persona_id":7})] {
        let response = request(&app, "POST", "/v1/sessions", invalid).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    {
        let inner = server.state.inner.lock().unwrap();
        assert_eq!((inner.sessions.len(), inner.events.len()), before);
    }
    for (index, branch) in ["child", "grandchild"].into_iter().enumerate() {
        let previous = read_json(get(&app, &format!("/v1/sessions/{parent}")).await).await;
        assert!(previous["last_event_id"].is_string());
        let response = request(
            &app,
            "POST",
            &format!("/v1/sessions/{parent}/fork"),
            json!({"branch_id":branch, "metadata":{"fork_canary":branch}}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let child = read_json(response).await;
        let id = child["id"].as_str().unwrap().to_owned();
        assert_eq!(child["transcript"]["session_id"], id);
        assert_ne!(child["last_event_id"], previous["last_event_id"]);
        let mut events = server.state.events_tx.subscribe();
        submit_completed_task(&app, &mut events, &id, "fork-provider-canary").await;
        assert_eq!(
            std::fs::read_to_string(root.path().join("task-retry-provider.txt")).unwrap(),
            "provider-reached\n".repeat(index + 2),
            "each accepted fork must reach a provider and complete"
        );
        drop(app);
        drop(server);
        server = ApiServer::new(config.clone());
        app = api_router(server.state.clone());
        let restored = read_json(get(&app, &format!("/v1/sessions/{id}")).await).await;
        assert_eq!(restored["id"], id);
        assert_eq!(restored["parent_session_id"], parent);
        assert_eq!(restored["root_session_id"], original);
        assert_eq!(restored["branch_id"], branch);
        assert_eq!(restored["transcript"], child["transcript"]);
        assert_eq!(restored["metadata"], child["metadata"]);
        let listed = read_json(get(&app, "/v1/sessions").await).await;
        let listed = listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == id)
            .unwrap();
        assert_eq!(listed, &restored);
        parent = id;
    }
}

async fn first_frame(
    response: Response,
) -> (
    Value,
    impl Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let frame = stream.next().await.unwrap().unwrap();
    let frame = std::str::from_utf8(&frame).unwrap();
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data:"))
        .unwrap();
    (serde_json::from_str(data.trim()).unwrap(), stream)
}

#[tokio::test]
async fn standard_sse_cursor_resumes_known_history_and_closes_unavailable_cursors() {
    let root = tempfile::tempdir().unwrap();
    let (config, session, task) = completed_session(root.path()).await;
    let server = ApiServer::new(config);
    let state = server.state.clone();
    let app = api_router(state.clone());
    let history = read_json(get(&app, &format!("/v1/tasks/{task}/events")).await).await;
    let events = history["data"].as_array().unwrap();
    assert!(events.len() >= 2);
    let cursor = events[0]["id"].as_str().unwrap();
    assert!(events[0]["sequence"].as_u64().unwrap() > 0);
    for path in [
        "/v1/events/stream".to_owned(),
        format!("/v1/sessions/{session}/events/stream"),
        format!("/v1/tasks/{task}/stream"),
    ] {
        for uri in [path.clone(), format!("{path}?after={cursor}")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("Last-Event-ID", cursor)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let (event, _) = first_frame(response).await;
            assert_eq!(event, events[1]);
            assert_ne!(
                event["id"], cursor,
                "the first replay frame must follow the known cursor"
            );
        }
        for unavailable in ["", "not-an-event", "event_ffffffffffffffff"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&path)
                        .header("Last-Event-ID", unavailable)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let (error, mut stream) = first_frame(response).await;
            assert_eq!(error["error"]["code"], "cursor_unavailable");
            assert!(
                stream.next().await.is_none(),
                "unavailable cursors must close after the error"
            );
        }
        let response = get(&app, &format!("{path}?after={cursor}")).await;
        let (event, _) = first_frame(response).await;
        assert_eq!(
            event, events[1],
            "the existing query cursor remains supported"
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{path}?after=event_ffffffffffffffff"))
                    .header("Last-Event-ID", cursor)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (error, mut stream) = first_frame(response).await;
        assert_eq!(error["error"]["code"], "cursor_unavailable");
        assert!(stream.next().await.is_none());
    }
    let volatile = state
        .append_event_with_persistence(
            Some(session.clone()),
            None,
            "known.volatile",
            json!({}),
            EventPersistence::LiveProjection,
        )
        .unwrap();
    let restarted = api_router(
        ApiServer::new({
            let script = root.path().join("agent.harn");
            let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
            config.workspace_root = root.path().into();
            config
        })
        .state,
    );
    let response = restarted
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sessions/{session}/events/stream"))
                .header("Last-Event-ID", volatile.id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (error, mut stream) = first_frame(response).await;
    assert_eq!(error["error"]["code"], "cursor_unavailable");
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn restored_session_mutations_preserve_policy_metadata_and_actual_transcript() {
    let root = tempfile::tempdir().unwrap();
    let (config, session, _) = completed_session(root.path()).await;
    let server = ApiServer::new(config.clone());
    let state = server.state.clone();
    let app = api_router(state.clone());
    assert_eq!(
        get(&app, &format!("/v1/sessions/{session}")).await.status(),
        StatusCode::OK
    );
    let patch = json!({"metadata":{"restored_canary":"kept"}, "summary":"known summary", "model_policy":{"provider":" MOCK ","model":" o3 ","reasoning_effort":"high"}});
    let response = request(&app, "PATCH", &format!("/v1/sessions/{session}"), patch).await;
    let status = response.status();
    let updated = read_json(response).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["metadata"], json!({"restored_canary":"kept"}));
    assert_eq!(updated["summary"], "known summary");
    assert_eq!(
        updated["model_policy"],
        json!({"provider":"mock", "model":"o3", "reasoning_effort":"high"})
    );
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/messages"),
        json!({"message":{"text":"restored-transcript-canary"}, "run":true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read_to_string(root.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\nprovider-reached\n"
    );
    let store = state.canonical_sessions.as_ref().unwrap();
    let transcript = store.read_all(&session).await.unwrap();
    assert!(
        !transcript.is_empty(),
        "the actual ACP transcript store must be reached"
    );
    assert!(serde_json::to_string(&transcript)
        .unwrap()
        .contains("restored-transcript-canary"));
    let restarted = ApiServer::new(config.clone());
    let restarted_state = restarted.state.clone();
    let app = api_router(restarted_state.clone());
    let fetched = read_json(get(&app, &format!("/v1/sessions/{session}")).await).await;
    assert_eq!(fetched["metadata"], updated["metadata"]);
    assert_eq!(fetched["summary"], updated["summary"]);
    assert_eq!(fetched["model_policy"], updated["model_policy"]);
    assert_eq!(fetched["updated_at"], updated["updated_at"]);
    let transcript = restarted_state
        .canonical_sessions
        .as_ref()
        .unwrap()
        .read_all(&session)
        .await
        .unwrap();
    assert!(serde_json::to_string(&transcript)
        .unwrap()
        .contains("restored-transcript-canary"));
    let replay_before = harn_vm::agent_session_restore::load_canonical_session_replay_from_store(
        restarted_state
            .canonical_sessions
            .as_ref()
            .unwrap()
            .as_ref(),
        &session,
    )
    .await
    .unwrap()
    .expect("canonical transcript replay");
    assert!(
        !replay_before.events.is_empty(),
        "the replay path must be reached"
    );
    assert!(
        replay_before.transcript_message_count > 1,
        "canonical transcript rows must contain messages beyond the retained prefix"
    );
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/truncate"),
        json!({"keep_first":1}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let truncated = read_json(response).await;
    assert_eq!(truncated["kept_turn_count"], 1);
    let replay_after = harn_vm::agent_session_restore::load_canonical_session_replay_from_store(
        restarted_state
            .canonical_sessions
            .as_ref()
            .unwrap()
            .as_ref(),
        &session,
    )
    .await
    .unwrap()
    .expect("canonical transcript replay after truncate");
    assert_eq!(replay_after.transcript_message_count, 1);
    assert_eq!(
        truncated["removed_turn_count"],
        replay_before.transcript_message_count - replay_after.transcript_message_count,
        "truncate's reported count matches the canonical transcript rows it removed"
    );
    let app = api_router(ApiServer::new(config.clone()).state);
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/truncate"),
        json!({"keep_first":1}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        read_json(response).await["removed_turn_count"],
        0,
        "the retained prefix survives a cold restore"
    );
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/messages"),
        json!({"message":{"text":"after-truncate-canary"}, "run":true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let provider_request =
        std::fs::read_to_string(root.path().join("task-retry-provider-request.json")).unwrap();
    assert!(provider_request.contains("initial-transcript-canary"));
    assert!(provider_request.contains("after-truncate-canary"));
    assert!(
        !provider_request.contains("restored-transcript-canary"),
        "the removed message must not reach the next provider request"
    );
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/close"),
        json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["state"], "CLOSED");
    let app = api_router(ApiServer::new(config).state);
    assert_eq!(
        read_json(get(&app, &format!("/v1/sessions/{session}")).await).await["state"],
        "CLOSED"
    );
    let response = request(
        &app,
        "POST",
        &format!("/v1/sessions/{session}/tasks"),
        json!({"input":"must not run after close"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(read_json(response).await["error"]["code"], "session_closed");
    assert_eq!(
        std::fs::read_to_string(root.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\nprovider-reached\nprovider-reached\n"
    );
}

#[tokio::test]
async fn canonical_truncate_refuses_active_writers_and_failed_persistence() {
    for active_writer in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (config, session, _) = completed_session(root.path()).await;
        let server = ApiServer::new(config.clone());
        drop(server.state.restore_session(&session).await.unwrap());
        let store = server.state.canonical_sessions.as_ref().unwrap();
        let before = store.read_all(&session).await.unwrap();
        assert!(!before.is_empty());
        let message_count = completed_prompt_message_count(root.path());
        let path = harn_vm::open_canonical_store(root.path())
            .unwrap()
            .path()
            .to_owned();
        let connection = rusqlite::Connection::open(&path).unwrap();
        let writer = if active_writer {
            Some(
                harn_session_store::SessionWriteLease::try_acquire(
                    path.parent().unwrap(),
                    &session,
                )
                .unwrap(),
            )
        } else {
            connection.execute_batch("CREATE TRIGGER reject_transcript_replace BEFORE INSERT ON session_events WHEN NEW.kind = 'compaction' BEGIN SELECT RAISE(FAIL, 'fixture transcript failure'); END;").unwrap();
            None
        };
        let app = api_router(server.state.clone());
        let path = format!("/v1/sessions/{session}/truncate");
        let response = request(&app, "POST", &path, json!({"keep_first":0})).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(read_json(response).await["error"]["code"], "acp_error");
        let unchanged = store.read_all(&session).await.unwrap();
        assert_eq!(
            serde_json::to_value(&unchanged).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        drop(writer);
        if !active_writer {
            connection
                .execute_batch("DROP TRIGGER reject_transcript_replace")
                .unwrap();
        }
        let response = request(&app, "POST", &path, json!({"keep_first":0})).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            read_json(response).await["removed_turn_count"],
            message_count,
            "truncate removes the actual provider context and reply"
        );
        let app = api_router(ApiServer::new(config).state);
        let response = request(&app, "POST", &path, json!({"keep_first":0})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let restored = read_json(response).await;
        assert_eq!(restored["kept_turn_count"], 0);
        assert_eq!(restored["removed_turn_count"], 0);
    }
}

async fn failed_policy_update_rolls_back(storage_failure: bool) {
    let root = tempfile::tempdir().unwrap();
    let (config, session, _) = completed_session(root.path()).await;
    let server = ApiServer::new(config);
    let mut state = server.state;
    let control = state
        .restore_session(&session)
        .await
        .unwrap_or_else(|error| panic!("known session restores: {error:?}"));
    drop(control);
    let store = state.task_store().unwrap();
    let before = store.get_task_session(&session).unwrap();
    assert!(before.model_policy.is_none());
    if storage_failure {
        let path = harn_vm::open_canonical_store(root.path())
            .unwrap()
            .path()
            .to_owned();
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap();
        conn.execute_batch("CREATE TRIGGER reject_descriptor_update BEFORE UPDATE ON agent_task_sessions BEGIN SELECT RAISE(FAIL, 'fixture descriptor failure'); END;").unwrap();
    }
    let acp = state.acp.clone();
    let (tx, mut requests) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    let app = api_router(state.clone());
    let path = format!("/v1/sessions/{session}");
    let update = tokio::spawn(async move {
        request(&app, "PATCH", &path, json!({"metadata":{"must_not_commit":true}, "model_policy":{"provider":"mock", "model":"o3", "reasoning_effort":"high"}})).await
    });
    for (index, config_id, value) in [
        (0, "model", "mock:o3"),
        (1, "thought_level", "high"),
        (2, "model", "@inherit"),
        (3, "thought_level", "@inherit"),
    ] {
        let rpc = requests.recv().await.unwrap();
        assert_eq!(rpc["method"], "session/set_config_option");
        assert_eq!(rpc["params"]["configId"], config_id);
        assert_eq!(rpc["params"]["value"], value);
        if index == 1 && !storage_failure {
            acp.resolve_response(json!({"id":rpc["id"], "error":{"code":-32602,"message":"fixture thought-level rejection"}})).await;
        } else {
            acp.resolve_response(json!({"id":rpc["id"], "result":{}}))
                .await;
        }
    }
    let response = update.await.unwrap();
    assert_eq!(
        response.status(),
        if storage_failure {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::BAD_GATEWAY
        }
    );
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task_session(&session)
            .unwrap(),
        before
    );
    assert!(state
        .session_resource(&session)
        .unwrap()
        .get("model_policy")
        .is_none());
    assert_eq!(
        state.session_resource(&session).unwrap()["metadata"],
        json!({})
    );
}

#[tokio::test]
async fn partial_policy_acknowledgement_rolls_back_before_publication() {
    failed_policy_update_rolls_back(false).await;
}

#[tokio::test]
async fn descriptor_write_failure_rolls_back_acknowledged_policy() {
    failed_policy_update_rolls_back(true).await;
}
