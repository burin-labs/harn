use super::*;

struct CaptureRuntimeLog(mpsc::UnboundedSender<Arc<AnyEventLog>>);

#[async_trait::async_trait(?Send)]
impl crate::adapters::acp::AcpRuntimeConfigurator for CaptureRuntimeLog {
    fn configure_harness(&self, harness: harn_vm::Harness) -> harn_vm::Harness {
        self.0
            .send(harn_vm::event_log::active_event_log().expect("served runtime event log"))
            .unwrap();
        harness
    }
}

async fn post(app: &Router, path: &str, input: Value) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn running_task(
    root: &Path,
) -> (
    ApiState,
    Router,
    String,
    String,
    broadcast::Receiver<ApiEvent>,
    Arc<AnyEventLog>,
) {
    let script = root.join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_input_gate.harn")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.into();
    config.default_session_mode = "code".into();
    let (log_tx, mut logs) = mpsc::unbounded_channel();
    config.acp.runtime_configurator = Arc::new(CaptureRuntimeLog(log_tx));
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let mut events = state.events_tx.subscribe();
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/tasks"),
        json!({"input":"hold the task"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    // Capture the actual served VM log rather than another SQLite connection's
    // independent in-process broadcast handle. Subscription also reads history.
    let log = logs.recv().await.unwrap();
    let mut gate = log
        .clone()
        .subscribe(
            &Topic::new("waitpoint.state.hitl_question_task_input_gate").unwrap(),
            None,
        )
        .await
        .unwrap();
    loop {
        tokio::select! {
            record = gate.next() => {
                let (_, record) = record.unwrap().unwrap();
                if record.payload["status"] == "open" { break; }
            }
            event = events.recv() => {
                let event = event.unwrap();
                assert_ne!(event.event, "task.failed", "{}", event.payload);
            }
        }
    }
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .get_task(&task_id)
            .unwrap()
            .status
            .as_str(),
        "WORKING"
    );
    (state, app, session_id, task_id, events, log)
}

async fn finish(log: &Arc<AnyEventLog>, task_id: &str, events: &mut broadcast::Receiver<ApiEvent>) {
    harn_vm::event_log::install_active_event_log(log.clone());
    harn_vm::append_hitl_response(
        None,
        serde_json::from_value(json!({
            "request_id":"hitl_question_task_input_gate", "answer":"done", "reviewer":"test",
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    loop {
        let event = events.recv().await.unwrap();
        if event.task_id.as_deref() == Some(task_id) && event.event == "task.completed" {
            break;
        }
        assert_ne!(event.event, "task.failed", "{}", event.payload);
    }
}

#[tokio::test]
async fn task_input_store_failure_rejects_before_queueing_and_audit() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, session_id, task_id, mut events, log) = running_task(root.path()).await;
    let before = state
        .task_store()
        .unwrap()
        .task_events(Some(&task_id), None)
        .unwrap();
    let store = harn_vm::open_canonical_store(root.path()).unwrap();
    let conn = rusqlite::Connection::open_with_flags(
        store.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    let stored_task: String = conn
        .query_row(
            "SELECT id FROM agent_tasks WHERE id = ?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_task, task_id,
        "fault injection must reach the actual task store"
    );
    let next_id: i64 = conn
        .query_row(
            "SELECT next_id FROM agent_control_event_sequence WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE agent_control_event_sequence SET next_id = ?1 WHERE singleton = 1",
        [i64::MAX],
    )
    .unwrap();
    let response = post(
        &app,
        &format!("/v1/tasks/{task_id}/messages"),
        json!({"kind":"input","message":{"text":"must not queue"}}),
    )
    .await;
    let status = response.status();
    let error = read_json(response).await;
    let after = state
        .task_store()
        .unwrap()
        .task_events(Some(&task_id), None)
        .unwrap();
    conn.execute(
        "UPDATE agent_control_event_sequence SET next_id = ?1 WHERE singleton = 1",
        [next_id],
    )
    .unwrap();
    finish(&log, &task_id, &mut events).await;
    let pending = state
        .acp
        .call(
            "session/pending_injections",
            json!({"sessionId":session_id}),
        )
        .await
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "task_store_unavailable");
    assert_eq!(
        after, before,
        "failed admission must not add a successful message audit"
    );
    assert_eq!(
        pending["pendingCount"], 0,
        "store failure must not leave an unrecorded input queued: {pending}"
    );
}

#[tokio::test]
async fn task_input_admission_and_publication_survive_http_caller_drop() {
    let root = tempfile::tempdir().unwrap();
    let (mut state, _, session_id, task_id, mut events, log) = running_task(root.path()).await;
    let original = state.acp.request_tx.clone();
    let (tx, mut requests) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    let app = api_router(state.clone());
    let path = format!("/v1/tasks/{task_id}/messages");
    let mut answer = Box::pin(post(
        &app,
        &path,
        json!({"kind":"input","message":{"text":"continue after disconnect","metadata":{"proof":"owned-admission"}}}),
    ));
    let request = tokio::select! {
        request = requests.recv() => request.unwrap(),
        response = &mut answer => panic!("HTTP response completed before ACP input delivery: {:?}", response.status()),
    };
    assert_eq!(request["method"], "session/inject");
    assert_eq!(request["params"]["content"], "continue after disconnect");
    drop(answer);
    state.acp.request_tx = original.clone();
    original.send(request).unwrap();
    let accepted = loop {
        let event = events.recv().await.unwrap();
        if event.event == "message.created"
            && event.payload["metadata"]["proof"] == "owned-admission"
        {
            break event;
        }
        assert_ne!(event.event, "task.failed", "{}", event.payload);
    };
    let message_id = accepted.payload["id"].as_str().unwrap().to_owned();
    assert!(message_id.starts_with("msg_inj_"));
    assert_eq!(accepted.task_id.as_deref(), Some(task_id.as_str()));
    let records = state
        .task_store()
        .unwrap()
        .task_events(Some(&task_id), None)
        .unwrap();
    let injected: Vec<_> = records
        .iter()
        .filter(|event| event.payload["id"] == message_id)
        .collect();
    assert_eq!(injected.len(), 1);
    assert_eq!(injected[0].id, accepted.id);
    finish(&log, &task_id, &mut events).await;
    let pending = state
        .acp
        .call(
            "session/pending_injections",
            json!({"sessionId":session_id}),
        )
        .await
        .unwrap();
    assert_eq!(pending["pendingCount"], 0);
}

#[tokio::test]
async fn configured_task_admission_preserves_non_task_session_execution() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    config.default_session_mode = "code".into();
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/messages"),
        json!({"message":{"text":"ordinary session turn"},"run":true}),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        read_json(response).await
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\n"
    );
    assert!(state
        .task_store()
        .unwrap()
        .list_tasks(None, None)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn task_prompt_admission_rejects_cancellation_after_rpc_enqueue() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    config.default_session_mode = "code".into();
    let mut state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let original = state.acp.request_tx.clone();
    let sessions = state.canonical_sessions.clone().unwrap();
    let store = sessions.task_store().unwrap();
    let (tx, mut requests) = mpsc::unbounded_channel();
    state.acp.request_tx = tx;
    let app = api_router(state.clone());
    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/tasks"),
        json!({"input":"must not execute"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    let prompt = requests.recv().await.unwrap();
    assert_eq!(prompt["method"], "session/prompt");
    assert_eq!(prompt["params"]["_meta"]["harn"]["task"]["taskId"], task_id);
    let canceled = post(&app, &format!("/v1/tasks/{task_id}/cancel"), json!({})).await;
    assert_eq!(canceled.status(), StatusCode::OK);
    let cancel = requests.recv().await.unwrap();
    assert_eq!(cancel["method"], "session/cancel");
    drop(app);
    drop(state);
    original.send(cancel).unwrap();
    original.send(prompt).unwrap();
    // Dispatch owns the last spy sender until the real ACP reply settles.
    assert!(requests.recv().await.is_none());
    assert_eq!(
        store.get_task(&task_id).unwrap().status,
        harn_session_store::task::TaskStatus::Canceled
    );
    assert!(
        !root.path().join("task-retry-provider.txt").exists(),
        "canceled prompt reached actual provider execution"
    );
}
