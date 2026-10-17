use super::*;
use harn_session_store::task::{
    AdmitTask, NewTaskControlEvent, StoredTaskRetry, TaskRecord, TaskSessionDescriptor, TaskStatus,
};
use harn_vm::task_retry::{
    TaskRequestFingerprint, TaskRetryActor, TaskRetryKey, TaskRetryRequest, TaskRetryScope,
};

pub(super) fn admission(id: &str, root: &Path, retry: Option<StoredTaskRetry>) -> AdmitTask {
    AdmitTask {
        task: TaskRecord {
            id: id.into(),
            session_id: "session-persisted".into(),
            workspace_id: "local".into(),
            created_at: "2026-10-11T00:00:00Z".into(),
            updated_at: "2026-10-11T00:00:00Z".into(),
            status: TaskStatus::Working,
            input: json!("hello"),
            metadata: json!({}),
            created_by: "api".into(),
            persona_id: None,
            branch_id: None,
            parent_task_id: None,
            assigned_agent_id: None,
            receipt_id: None,
            outcome_id: None,
            quota_id: None,
            started_at: Some("2026-10-11T00:00:00Z".into()),
            completed_at: None,
            canceled_at: None,
            failure: None,
        },
        session: TaskSessionDescriptor {
            id: "session-persisted".into(),
            workspace_id: "local".into(),
            workspace_root: root.to_string_lossy().into(),
            mode_id: "code".into(),
            model_policy: None,
            metadata: json!({}),
            persona_id: None,
            root_session_id: None,
            parent_session_id: None,
            branch_id: None,
            created_at: "2026-10-11T00:00:00Z".into(),
            updated_at: "2026-10-11T00:00:00Z".into(),
            summary: None,
            status: harn_session_store::SessionStatus::Open,
        },
        retry,
        initial_events: vec![NewTaskControlEvent {
            event: "message.created".into(),
            payload: json!({"id":"message-first","text":"hello"}),
        }],
        // Keep this fixture's binding live independently of the wall clock.
        now_ms: i64::MAX,
    }
}

fn config(root: &Path, script: &Path) -> ApiServerConfig {
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.into();
    config
}

async fn post(app: &Router, path: &str, key: Option<&str>, input: Value) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    app.clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&input).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn terminal(events: &mut broadcast::Receiver<ApiEvent>, id: &str) -> ApiEvent {
    loop {
        let event = events.recv().await.expect("terminal task event");
        if event.task_id.as_deref() == Some(id)
            && matches!(event.event.as_str(), "task.completed" | "task.failed")
        {
            return event;
        }
    }
}

#[tokio::test]
async fn task_retry_dispatches_once_and_reuses_durable_resource_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let server = ApiServer::new(config(dir.path(), &script));
    let state = server.state.clone();
    let mut events = state.events_tx.subscribe();
    let app = api_router(state.clone());
    let response = post(
        &app,
        "/v1/sessions",
        None,
        json!({"workspace_id":"local","mode_id":"code"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/sessions/{session}/tasks");
    let input = json!({"input":"hello","metadata":{"control":"one"}});
    let (first, retry) = tokio::join!(
        post(&app, &path, Some("same-key"), input.clone()),
        post(&app, &path, Some("same-key"), input.clone())
    );
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(retry.status(), StatusCode::ACCEPTED);
    let first = read_json(first).await;
    assert_eq!(read_json(retry).await["id"], first["id"]);
    let task_id = first["id"].as_str().unwrap();
    let finished = terminal(&mut events, task_id).await;
    assert_eq!(finished.event, "task.completed", "{}", finished.payload);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\n"
    );
    let before = state.task_store().unwrap().task_events(None, None).unwrap();
    let live_updates = state.inner.lock().unwrap().events.clone();
    assert!(
        live_updates
            .iter()
            .any(|event| event.task_id.as_deref() == Some(task_id)
                && event
                    .payload
                    .pointer("/update/sessionUpdate")
                    .and_then(Value::as_str)
                    == Some("agent_message_chunk")
                && event
                    .payload
                    .pointer("/update/content/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("provider-reached"))),
        "the streamed provider response must reach the live API projection"
    );
    assert!(
        before.iter().all(|event| event.event != "session.update"),
        "ACP transcript updates must not be persisted as task controls"
    );
    let conflict = post(&app, &path, Some("same-key"), json!({"input":"different"})).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        read_json(conflict).await["error"]["code"],
        "idempotency_key_reused"
    );
    assert_eq!(
        state.task_store().unwrap().task_events(None, None).unwrap(),
        before
    );
    let reopened = ApiServer::new(config(dir.path(), &script));
    assert!(
        !reopened
            .state
            .inner
            .lock()
            .unwrap()
            .sessions
            .contains_key(&session),
        "readback must reach the persisted descriptor"
    );
    let reopened_state = reopened.state.clone();
    let reopened_app = api_router(reopened.state);
    let fetched = get(&reopened_app, &format!("/v1/sessions/{session}")).await;
    assert_eq!(fetched.status(), StatusCode::OK);
    let persisted = read_json(fetched).await;
    let list = get(&reopened_app, "/v1/sessions").await;
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(read_json(list).await["data"], json!([persisted.clone()]));
    let mut live = persisted;
    live["summary"] = json!("newer live metadata");
    reopened_state
        .inner
        .lock()
        .unwrap()
        .sessions
        .insert(session.clone(), live.clone());
    let list = get(&reopened_app, "/v1/sessions").await;
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(
        read_json(list).await["data"],
        json!([live]),
        "live session overlays the persisted row once"
    );
    reopened_state
        .inner
        .lock()
        .unwrap()
        .sessions
        .remove(&session);
    let reused = post(
        &reopened_app,
        "/v1/tasks",
        Some("same-key"),
        json!({"session_id":session,"input":"hello","metadata":{"control":"one"}}),
    )
    .await;
    assert_eq!(reused.status(), StatusCode::ACCEPTED);
    let reused = read_json(reused).await;
    assert_eq!(reused["id"], first["id"]);
    assert_eq!(reused["status"], "COMPLETED");
    assert_eq!(
        reopened_state
            .task_store()
            .unwrap()
            .task_events(None, None)
            .unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\n"
    );
    let view = get(&reopened_app, &format!("/v1/sessions/{session}/view")).await;
    assert_eq!(view.status(), StatusCode::OK);
    let view = read_json(view).await;
    let cursor = before.last().unwrap().id.clone();
    assert_eq!(view["session"]["last_event_id"], event_seq(&cursor));
    let history = get(
        &reopened_app,
        &format!("/v1/tasks/{task_id}/events?after={}", before[0].id),
    )
    .await;
    assert_eq!(history.status(), StatusCode::OK);
    let history = read_json(history).await;
    let ids = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        before
            .iter()
            .skip(1)
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>()
    );
    // Removing the key must really admit and dispatch another provider call.
    let mut next_events = state.events_tx.subscribe();
    let fresh = post(&app, &path, None, input).await;
    assert_eq!(fresh.status(), StatusCode::ACCEPTED);
    let fresh = read_json(fresh).await;
    assert_ne!(fresh["id"], first["id"]);
    assert_eq!(
        terminal(&mut next_events, fresh["id"].as_str().unwrap())
            .await
            .event,
        "task.completed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("task-retry-provider.txt")).unwrap(),
        "provider-reached\nprovider-reached\n"
    );
}

#[tokio::test]
async fn task_retry_recovers_interrupted_execution_without_redispatching() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let input = json!({"session_id":"session-persisted","input":"hello"});
    let retry = TaskRetryRequest::new(
        TaskRetryScope::for_task_create(
            TaskRetryActor::new("none", "anonymous").unwrap(),
            Some("local"),
            "session-persisted",
            TaskRetryKey::parse("restart-key").unwrap(),
        )
        .unwrap(),
        TaskRequestFingerprint::from_value(&input),
    );
    let state = ApiServer::new(config(dir.path(), &script)).state;
    let store = state.task_store().unwrap();
    let live_claim = store.claim_task_execution("persisted").unwrap();
    store
        .admit_task(admission(
            "persisted",
            dir.path(),
            Some(StoredTaskRetry {
                binding_digest: retry.scope().digest().into(),
                request_fingerprint: retry.fingerprint().as_hex().into(),
            }),
        ))
        .unwrap();
    let second = ApiServer::new(config(dir.path(), &script)).state;
    second.recover_interrupted_tasks().unwrap();
    assert_eq!(
        second
            .task_store()
            .unwrap()
            .get_task("persisted")
            .unwrap()
            .status,
        TaskStatus::Working,
        "another live executor must not be interrupted"
    );
    drop(live_claim);
    drop(state);
    let app = api_router(second.clone());
    let response = post(&app, "/v1/tasks", Some("restart-key"), input).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task = read_json(response).await;
    assert_eq!(task["id"], "persisted");
    assert_eq!(task["status"], "FAILED");
    assert_eq!(task["failure"]["code"], "task_interrupted");
    assert!(!dir.path().join("task-retry-provider.txt").exists());
    let history = second
        .task_store()
        .unwrap()
        .task_events(None, None)
        .unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event == "message.created")
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event == "task.failed")
            .count(),
        1
    );
}

#[tokio::test]
async fn task_retry_rejects_present_invalid_and_unsupported_headers() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let app = api_router(ApiServer::new(config(dir.path(), &script)).state);
    let unsupported = post(&app, "/v1/sessions", Some("key"), json!({})).await;
    assert_eq!(unsupported.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        read_json(unsupported).await["error"]["code"],
        "idempotency_not_supported"
    );
    for key in ["", " ", "tab\t", "é"] {
        let invalid = post(&app, "/v1/tasks", Some(key), json!({})).await;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            read_json(invalid).await["error"]["code"],
            "invalid_idempotency_key"
        );
    }
    for path in ["/health", "/v1/tasks", "/v1/session-store/sessions"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("idempotency-key", "key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "unsupported route {path}"
        );
        assert_eq!(
            read_json(response).await["error"]["code"],
            "idempotency_not_supported"
        );
    }
    let duplicate = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/tasks")
                .header("idempotency-key", "one")
                .header("idempotency-key", "two")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        read_json(duplicate).await["error"]["code"],
        "invalid_idempotency_key"
    );
    let policy = AuthPolicy {
        methods: vec![crate::auth::AuthMethodConfig::ApiKey(
            crate::auth::ApiKeyAuthConfig::single("valid-key"),
        )],
        mcp_allowlist: None,
    };
    let protected =
        api_router(ApiServer::new(config(dir.path(), &script).with_auth_policy(policy)).state);
    let rejected = post(&protected, "/v1/tasks", Some("key"), json!({})).await;
    assert_eq!(
        rejected.status(),
        StatusCode::UNAUTHORIZED,
        "supported retry must reach authentication"
    );
    assert_eq!(
        read_json(rejected).await["error"]["code"],
        "unauthenticated"
    );
}

#[tokio::test]
async fn task_control_persistence_keeps_acp_transcript_updates_live_only() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let state = ApiServer::new(config(dir.path(), &script)).state;
    let claim = state
        .task_store()
        .unwrap()
        .claim_task_execution("live-task")
        .unwrap();
    state
        .task_store()
        .unwrap()
        .admit_task(admission("live-task", dir.path(), None))
        .unwrap();
    let before = state.task_store().unwrap().task_events(None, None).unwrap();
    for update in [
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"live-transcript-canary"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"live-tool","title":"live-transcript-canary","status":"in_progress"}),
    ] {
        state.handle_acp_notification(json!({"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"session-persisted","update":update,
            "_meta":{"harn":{"task":TaskPromptTarget {task_id:"live-task".into()}}}
        }}));
    }
    let live = state.inner.lock().unwrap().events.clone();
    assert_eq!(
        live.len(),
        2,
        "the actual notification owner published both updates"
    );
    assert!(live
        .iter()
        .all(|event| event.task_id.as_deref() == Some("live-task")));
    assert!(live.iter().all(|event| event.event == "session.update"));
    assert_eq!(
        state.task_store().unwrap().task_events(None, None).unwrap(),
        before
    );
    state
        .change_task("live-task", TaskStatus::Completed, None)
        .unwrap();
    drop(claim);
    let reopened = ApiServer::new(config(dir.path(), &script)).state;
    let persisted = reopened
        .task_store()
        .unwrap()
        .task_events(None, None)
        .unwrap();
    assert_eq!(persisted.len(), 3);
    assert_eq!(persisted[0].event, "task.started");
    assert_eq!(persisted[1].event, "message.created");
    assert_eq!(persisted[2].event, "task.completed");
    assert!(!serde_json::to_string(&persisted)
        .unwrap()
        .contains("live-transcript-canary"));
    assert!(event_seq(&persisted[2].id) > event_seq(&live[1].id));
}

#[tokio::test]
async fn task_submission_fails_closed_without_the_durable_owner() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
    let mut state = ApiServer::new(config(dir.path(), &script)).state;
    state.canonical_sessions = None;
    let app = api_router(state);
    let response = post(
        &app,
        "/v1/tasks",
        Some("key"),
        json!({"session_id":"unavailable","input":"hello"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        read_json(response).await["error"]["code"],
        "task_store_unavailable"
    );
    assert!(!dir.path().join("task-retry-provider.txt").exists());
}

#[tokio::test]
async fn first_task_control_or_session_read_after_restart_recovers_before_effects() {
    for operation in ["messages", "cancel", "session"] {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("agent.harn");
        std::fs::write(&script, include_str!("fixtures/task_retry.harn")).unwrap();
        let state = ApiServer::new(config(dir.path(), &script)).state;
        state
            .task_store()
            .unwrap()
            .admit_task(admission("interrupted", dir.path(), None))
            .unwrap();
        drop(state);
        let mut reopened = ApiServer::new(config(dir.path(), &script)).state;
        assert!(reopened.inner.lock().unwrap().sessions.is_empty());
        let (request_tx, mut requests) = mpsc::unbounded_channel();
        reopened.acp.request_tx = request_tx;
        let app = api_router(reopened.clone());
        let response = async {
            match operation {
                "messages" => {
                    post(
                        &app,
                        "/v1/tasks/interrupted/messages",
                        None,
                        json!({"kind":"input","message":"steer"}),
                    )
                    .await
                }
                "cancel" => post(&app, "/v1/tasks/interrupted/cancel", None, json!({})).await,
                "session" => get(&app, "/v1/sessions/session-persisted").await,
                _ => unreachable!(),
            }
        };
        let response = tokio::select! {
            response = response => response,
            effect = requests.recv() => panic!("{operation} dispatched an ACP effect before recovery: {effect:?}"),
        };
        if operation == "session" {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(read_json(response).await["state"], "IDLE");
        } else {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let code = if operation == "messages" {
                "task_terminal"
            } else {
                "task_not_cancelable"
            };
            assert_eq!(read_json(response).await["error"]["code"], code);
        }
        assert!(
            matches!(requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "no dispatch, inject, or cancel was queued"
        );
        let store = reopened.task_store().unwrap();
        let task = store.get_task("interrupted").unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.failure.unwrap().code, "task_interrupted");
        assert_eq!(
            store
                .task_events(None, None)
                .unwrap()
                .iter()
                .filter(|event| event.event == "task.failed")
                .count(),
            1
        );
        assert!(!dir.path().join("task-retry-provider.txt").exists());
    }

    // The same ready boundary must leave another live executor untouched.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let live = ApiServer::new(config(dir.path(), &script)).state;
    let claim = live
        .task_store()
        .unwrap()
        .claim_task_execution("live")
        .unwrap();
    live.task_store()
        .unwrap()
        .admit_task(admission("live", dir.path(), None))
        .unwrap();
    let before = live.task_store().unwrap().get_task("live").unwrap();
    let mut reopened = ApiServer::new(config(dir.path(), &script)).state;
    let (request_tx, mut requests) = mpsc::unbounded_channel();
    reopened.acp.request_tx = request_tx;
    let response = api_router(reopened.clone())
        .oneshot(
            Request::builder()
                .uri("/v1/sessions/session-persisted")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["state"], "WORKING");
    assert_eq!(
        reopened.task_store().unwrap().get_task("live").unwrap(),
        before
    );
    assert_eq!(
        reopened
            .task_store()
            .unwrap()
            .task_events(None, None)
            .unwrap()
            .len(),
        2
    );
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    // A live cancellation reaches this same effect detector, so empty above
    // proves a withheld operation rather than a disconnected instrument.
    let response = post(
        &api_router(reopened.clone()),
        "/v1/tasks/live/cancel",
        None,
        json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["status"], "CANCELED");
    let effect = requests.try_recv().expect("live cancellation reaches ACP");
    assert_eq!(effect["method"], "session/cancel");
    assert_eq!(effect["params"]["sessionId"], "session-persisted");
    drop(claim);
}

async fn message_texts(app: &Router, session_id: &str) -> Vec<String> {
    let response = get(app, &format!("/v1/sessions/{session_id}/messages")).await;
    assert_eq!(response.status(), StatusCode::OK);
    read_json(response).await["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["parts"][0]["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn task_messages_follow_mixed_control_order_and_truncation_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let state = ApiServer::new(config(dir.path(), &script)).state;
    let mut events = state.events_tx.subscribe();
    let app = api_router(state.clone());
    let response = post(
        &app,
        "/v1/sessions",
        None,
        json!({"workspace_id":"local","mode_id":"code"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session_id = read_json(response).await["id"].as_str().unwrap().to_owned();
    for (text, task) in [
        ("alpha", false),
        ("beta", true),
        ("gamma", false),
        ("delta", true),
    ] {
        let path = if task {
            format!("/v1/sessions/{session_id}/tasks")
        } else {
            format!("/v1/sessions/{session_id}/messages")
        };
        let input = if task {
            json!({"input":text})
        } else {
            json!({"message":text})
        };
        let response = post(&app, &path, None, input).await;
        if task {
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let task = read_json(response).await;
            assert_eq!(
                terminal(&mut events, task["id"].as_str().unwrap())
                    .await
                    .event,
                "task.completed"
            );
        } else {
            assert_eq!(response.status(), StatusCode::CREATED);
        }
    }
    assert_eq!(
        message_texts(&app, &session_id).await,
        ["alpha", "beta", "gamma", "delta"]
    );
    let audit = state
        .task_store()
        .unwrap()
        .task_events(None, Some(&session_id))
        .unwrap();
    let path = format!("/v1/sessions/{session_id}/truncate");
    let response = post(&app, &path, None, json!({"keep_first":2})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(message_texts(&app, &session_id).await, ["alpha", "beta"]);
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .task_events(None, Some(&session_id))
            .unwrap(),
        audit
    );
    let reopened = api_router(ApiServer::new(config(dir.path(), &script)).state);
    // Standalone API messages retain their existing process-local lifetime;
    // the audited task-message visibility boundary survives restart.
    assert_eq!(message_texts(&reopened, &session_id).await, ["beta"]);

    let response = post(
        &app,
        &format!("/v1/sessions/{session_id}/tasks"),
        None,
        json!({"input":"epsilon"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let task = read_json(response).await;
    assert_eq!(
        terminal(&mut events, task["id"].as_str().unwrap())
            .await
            .event,
        "task.completed"
    );
    assert_eq!(
        message_texts(&app, &session_id).await,
        ["alpha", "beta", "epsilon"]
    );
    let response = post(&app, &path, None, json!({"keep_first":0})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(message_texts(&app, &session_id).await.is_empty());
    let reopened = api_router(ApiServer::new(config(dir.path(), &script)).state);
    assert!(message_texts(&reopened, &session_id).await.is_empty());
    assert_eq!(
        state
            .task_store()
            .unwrap()
            .task_events(None, Some(&session_id))
            .unwrap()
            .iter()
            .filter(|event| event.event == "message.created")
            .count(),
        3
    );
}

#[tokio::test]
async fn standalone_message_projection_commits_with_its_ordering_event() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let mut state = ApiServer::new(config(dir.path(), &script)).state;
    let app = api_router(state.clone());
    let response = post(&app, "/v1/sessions", None, json!({"workspace_id":"local"})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/sessions/{session}/messages");
    let response = post(&app, &path, None, json!({"message":"published"})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(message_texts(&app, &session).await, ["published"]);
    {
        let inner = state.inner.lock().unwrap();
        let messages = &inner.messages[&session];
        assert_eq!(messages.len(), 1);
        assert_eq!(
            inner
                .events
                .iter()
                .filter(|event| event.event == "message.created" && event.payload == messages[0])
                .count(),
            1
        );
    }
    // A failed identity allocation cannot expose a message with no ordering event.
    state.canonical_sessions = None;
    let response = post(
        &api_router(state.clone()),
        &path,
        None,
        json!({"message":"unpublished"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.messages[&session].len(), 1);
    assert_eq!(
        inner
            .events
            .iter()
            .filter(|event| event.event == "message.created")
            .count(),
        1
    );
}

#[tokio::test]
async fn persisted_session_model_policy_accepts_only_the_closed_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.harn");
    write_test_pipeline(&script);
    let state = ApiServer::new(config(dir.path(), &script)).state;
    let app = api_router(state.clone());
    for field in ["api_key", "headers", "credentials", "endpoint"] {
        let mut policy = json!({"provider":"mock","model":"o3"});
        policy[field] = json!("policy-secret-canary");
        let response = post(&app, "/v1/sessions", None, json!({"model_policy":policy})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = read_json(response).await;
        assert_eq!(error["error"]["code"], "invalid_model_policy");
        assert!(!error.to_string().contains("policy-secret-canary"));
        assert!(
            state.inner.lock().unwrap().sessions.is_empty(),
            "invalid policy cannot create a session"
        );
    }
    let policy = json!({"provider":"mock","model":"o3","reasoning_effort":"high"});
    let response = post(&app, "/v1/sessions", None, json!({"model_policy":policy})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    assert_eq!(session["model_policy"], policy);
    assert_eq!(
        state
            .session_descriptor(session["id"].as_str().unwrap())
            .unwrap()
            .model_policy,
        Some(policy)
    );
}
