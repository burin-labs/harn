use super::*;

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_lists_and_loads_a_store_only_session_from_its_project() {
    let _guard = lock_harn_state_async().await;
    reset_active_event_log();

    let project = tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join(".harn")).expect("project state dir");
    let project_root = std::fs::canonicalize(project.path()).expect("canonical project root");
    let session_id = "01a003d0-1513-7271-90aa-4542d6059498";
    {
        use harn_serve::{
            AppendEvent, CreateSession, SessionEventKind, SessionStore, SqliteSessionStore,
        };
        let store = SqliteSessionStore::open(project.path().join(".harn/session-store.sqlite"))
            .expect("open canonical store");
        store
            .create(CreateSession {
                id: Some(session_id.to_string()),
                cwd: Some(project_root.display().to_string()),
                project_scope: Some(project_root.display().to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create stored session");
        store
            .append(
                session_id,
                AppendEvent::new(
                    SessionEventKind::Message,
                    json!({
                        "transcript_event": {
                            "kind": "message",
                            "role": "assistant",
                            "visibility": "public",
                            "text": "restored through the canonical store",
                        }
                    }),
                ),
            )
            .await
            .expect("append stored transcript");
    }

    let (listener, _log, _listener_dir) = start_acp_test_listener_with_project_root(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
        Some(&project_root),
    )
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("connect");
    let cwd = project_root.display().to_string();
    let listed = acp_request(
        &mut socket,
        1,
        "session/list",
        json!({"cwd": cwd.clone(), "liveState": "persisted"}),
    )
    .await;

    send_acp_request(
        &mut socket,
        2,
        "session/load",
        json!({"sessionId": session_id, "cwd": cwd}),
    )
    .await;
    let mut replayed_text = String::new();
    let loaded = loop {
        let message = next_acp_text(&mut socket).await;
        if message.get("id").and_then(JsonValue::as_u64) == Some(2) {
            break message;
        }
        replayed_text.push_str(&message.to_string());
    };

    assert_eq!(
        listed["result"]["sessions"][0]["sessionId"],
        json!(session_id),
        "session/list must use the same canonical store as session/load: {listed}"
    );
    assert_eq!(
        listed["result"]["sessions"][0]["liveState"],
        json!("persisted")
    );
    assert!(
        loaded.get("error").is_none(),
        "store-only session must load over the WebSocket transport: {loaded}"
    );
    assert_eq!(loaded["result"]["session"]["sessionId"], json!(session_id));
    assert!(
        replayed_text.contains("restored through the canonical store"),
        "the durable transcript must be replayed, got {replayed_text}"
    );

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();

    harn_vm::agent_sessions::close(session_id);
    let local = tokio::task::LocalSet::new();
    let (in_process, in_process_replayed_text) = local
        .run_until(async {
            let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(harn_serve::run_acp_channel_server(
                harn_serve::AcpServerConfig::new(None),
                request_rx,
                response_tx,
            ));
            request_tx
                .send(json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/load",
                    "params": {"sessionId": session_id, "cwd": project_root},
                }))
                .expect("send in-process session/load");
            let mut replayed_text = String::new();
            let response = loop {
                let line = response_rx.recv().await.expect("in-process response");
                let message: JsonValue = serde_json::from_str(&line).expect("in-process json");
                if message.get("id").and_then(JsonValue::as_u64) == Some(3) {
                    break message;
                }
                replayed_text.push_str(&message.to_string());
            };
            drop(request_tx);
            server.await.expect("in-process ACP server");
            (response, replayed_text)
        })
        .await;

    let mut websocket_session = loaded["result"]["session"].clone();
    let mut in_process_session = in_process["result"]["session"].clone();
    for session in [&mut websocket_session, &mut in_process_session] {
        assert!(session["createdAt"].is_string());
        session
            .as_object_mut()
            .expect("session object")
            .remove("createdAt");
    }
    assert_eq!(
        websocket_session, in_process_session,
        "WebSocket and in-process session/load must return the same session shape"
    );
    assert!(
        in_process_replayed_text.contains("restored through the canonical store"),
        "in-process load must replay the same durable transcript: {in_process_replayed_text}"
    );
    harn_vm::agent_sessions::close(session_id);
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_rejects_another_existing_project_store() {
    let _guard = lock_harn_state_async().await;
    reset_active_event_log();

    let owning_project = tempdir().expect("owning project root");
    let other_project = tempdir().expect("other project root");
    std::fs::create_dir_all(owning_project.path().join(".harn")).expect("owning project state dir");
    std::fs::create_dir_all(other_project.path().join(".harn")).expect("other project state dir");
    let owning_project_root =
        std::fs::canonicalize(owning_project.path()).expect("canonical owning project root");
    let other_project_root =
        std::fs::canonicalize(other_project.path()).expect("canonical other project root");
    let session_id = "01a003d0-1513-7271-90aa-4542d6059499";
    {
        use harn_serve::{CreateSession, SessionStore, SqliteSessionStore};
        let store =
            SqliteSessionStore::open(other_project.path().join(".harn/session-store.sqlite"))
                .expect("open other project store");
        store
            .create(CreateSession {
                id: Some(session_id.to_string()),
                cwd: Some(other_project_root.display().to_string()),
                project_scope: Some(other_project_root.display().to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create stored session");
    }

    let (listener, _log, _listener_dir) = start_acp_test_listener_with_project_root(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
        Some(&owning_project_root),
    )
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("connect");
    let other_cwd = other_project_root.display().to_string();
    let listed = acp_request(
        &mut socket,
        1,
        "session/list",
        json!({"cwd": other_cwd.clone(), "liveState": "persisted"}),
    )
    .await;
    let loaded = acp_request(
        &mut socket,
        2,
        "session/load",
        json!({"sessionId": session_id, "cwd": other_cwd}),
    )
    .await;

    for denied in [listed, loaded] {
        assert_eq!(denied["error"]["code"], json!(-32001));
        assert_eq!(
            denied["error"]["data"]["reason"],
            json!("project_root_not_authorized"),
            "another existing project's store must be denied before it is opened: {denied}"
        );
    }

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}
#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_cold_load_requires_an_existing_project_root() {
    let _guard = lock_harn_state_async().await;
    reset_active_event_log();

    let (listener, _log, _listener_dir) = start_acp_test_listener_with_env(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
    )
    .await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("connect");

    let missing = acp_request(
        &mut socket,
        1,
        "session/load",
        json!({"sessionId": "missing-session"}),
    )
    .await;
    assert_eq!(missing["error"]["code"], json!(-32602));
    assert!(
        missing["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("cwd is required")),
        "missing scope must be distinct from an unknown session: {missing}"
    );

    let absent_root = std::env::temp_dir().join(format!("harn-missing-{}", uuid::Uuid::now_v7()));
    let invalid = acp_request(
        &mut socket,
        2,
        "session/load",
        json!({
            "sessionId": "missing-session",
            "cwd": absent_root.display().to_string(),
        }),
    )
    .await;
    assert_eq!(invalid["error"]["code"], json!(-32602));
    assert!(
        invalid["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("does not select a project directory")),
        "invalid scope must be distinct from an unknown session: {invalid}"
    );

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}
