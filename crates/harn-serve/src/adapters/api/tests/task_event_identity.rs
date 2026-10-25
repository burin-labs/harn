use super::*;
use harn_session_store::task::TaskStatus;
use std::sync::atomic::{AtomicU64, Ordering};

struct GateBeforeApproval {
    first: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    turns: AtomicU64,
    raw_permission: bool,
    bridges: mpsc::UnboundedSender<(
        u64,
        harn_vm::bridge::HostBridgeInjectionState,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
}

#[async_trait::async_trait(?Send)]
impl crate::adapters::acp::AcpRuntimeConfigurator for GateBeforeApproval {
    async fn run_prompt(
        &self,
        context: crate::adapters::acp::AcpPromptExecutionContext<'_>,
        execution: crate::adapters::acp::AcpPromptExecution<'_>,
    ) -> Result<String, crate::adapters::acp::AcpPromptExecutionError> {
        let turn = self.turns.fetch_add(1, Ordering::SeqCst);
        assert!(self
            .bridges
            .send((
                turn,
                context.host_bridge.injection_state(),
                context.host_bridge.cancelled_flag()
            ))
            .is_ok());
        let gate = self.first.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            assert!(entered.send(()).is_ok());
            release.await.unwrap();
        }
        if self.raw_permission {
            let result = context
                .host_bridge
                .call(
                    "session/request_permission",
                    json!({
                        "toolCall":{"toolCallId":format!("task-permission-{turn}"), "title":"continue"},
                        "options":[{"optionId":"allow", "name":"Allow", "kind":"allow_once"}],
                    }),
                )
                .await;
            if !context.is_cancelled() {
                assert_eq!(result.unwrap()["outcome"]["optionId"], "allow");
            }
        }
        execution.await
    }
}

async fn next_permission(events: &mut broadcast::Receiver<ApiEvent>) -> ApiEvent {
    loop {
        let event = events.recv().await.unwrap();
        assert_ne!(event.event, "task.failed", "{}", event.payload);
        if event.event == "permission.requested" {
            return event;
        }
    }
}

#[derive(Clone, Copy)]
enum SteeringScenario {
    None,
    QueuedTask,
    AcrossTurns,
    CancelQueued,
    CancelRunning,
}

async fn queued_task_does_not_steal_running_approval(
    raw_permission: bool,
    steering: SteeringScenario,
) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    if raw_permission {
        write_test_pipeline(&script);
    } else {
        std::fs::write(&script, include_str!("fixtures/task_approval.harn")).unwrap();
    }
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let (bridges, mut runtime_bridges) = mpsc::unbounded_channel();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned());
    config.workspace_root = root.path().into();
    config.default_session_mode = "code".into();
    let configurator = Arc::new(GateBeforeApproval {
        first: Mutex::new(Some((entered_tx, release_rx))),
        turns: AtomicU64::new(0),
        raw_permission,
        bridges,
    });
    config.acp.runtime_configurator = configurator.clone();
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let mut events = state.events_tx.subscribe();
    let session = post(&app, "/v1/sessions", json!({"workspace_id":"local"})).await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let route = format!("/v1/sessions/{session_id}/tasks");
    let first = post(&app, &route, json!({"input":"first task"})).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_id = read_json(first).await["id"].as_str().unwrap().to_owned();
    entered.await.unwrap();
    let (turn, first_inputs, first_cancelled) = runtime_bridges.recv().await.unwrap();
    assert_eq!(turn, 0);
    let second = post(&app, &route, json!({"input":"second task"})).await;
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    let second_id = read_json(second).await["id"].as_str().unwrap().to_owned();
    let store = state.task_store().unwrap();
    assert_eq!(
        store.get_task(&first_id).unwrap().status,
        TaskStatus::Working
    );
    assert_eq!(
        store.get_task(&second_id).unwrap().status,
        TaskStatus::Submitted
    );
    assert_eq!(configurator.turns.load(Ordering::SeqCst), 1);
    let mut accepted_message = None;
    if matches!(steering, SteeringScenario::QueuedTask) {
        let before = store.task_events(Some(&second_id), None).unwrap();
        let response = post(
            &app,
            &format!("/v1/tasks/{second_id}/messages"),
            json!({"kind":"input", "message":{"text":"queued-task-canary"}}),
        )
        .await;
        let status = response.status();
        let body = read_json(response).await;
        let pending = first_inputs.pending_injections_json().await;
        let after = store.task_events(Some(&second_id), None).unwrap();
        assert_eq!(status, StatusCode::CONFLICT,
            "wrong task response={body}; first runtime queue={pending}; queued task audit={after:?}");
        assert_eq!(body["error"]["code"], "task_not_steerable");
        assert_eq!(pending["pendingCount"], 0);
        assert_eq!(after, before);
    }
    if matches!(
        steering,
        SteeringScenario::QueuedTask | SteeringScenario::AcrossTurns
    ) {
        let response = post(
            &app,
            &format!("/v1/tasks/{first_id}/messages"),
            json!({"kind":"input", "message":{"text":"running-task-canary"}}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let message = read_json(response).await;
        let pending = first_inputs.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 1);
        assert_eq!(
            pending["injections"][0]["content"][0]["text"],
            "running-task-canary"
        );
        assert_eq!(pending["injections"][0]["messageId"], message["id"]);
        let audit = store.task_events(Some(&first_id), None).unwrap();
        assert!(audit
            .iter()
            .any(|event| event.event == "message.created" && event.payload == message));
        if matches!(steering, SteeringScenario::QueuedTask) {
            assert_eq!(
                first_inputs
                    .revoke_pending_user_message(message["id"].as_str().unwrap())
                    .await,
                harn_vm::bridge::PendingUserMessageMutationResult::Mutated
            );
        }
        accepted_message = Some(message);
    }
    assert!(release.send(()).is_ok());
    let permission = next_permission(&mut events).await;
    assert_eq!(permission.task_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(permission.payload["task_id"], first_id);
    assert_eq!(
        store.get_task(&first_id).unwrap().status,
        TaskStatus::AuthRequired
    );
    assert_eq!(
        store.get_task(&second_id).unwrap().status,
        TaskStatus::Submitted
    );
    if matches!(
        steering,
        SteeringScenario::CancelQueued | SteeringScenario::CancelRunning
    ) {
        let target = if matches!(steering, SteeringScenario::CancelQueued) {
            &second_id
        } else {
            &first_id
        };
        let response = post(&app, &format!("/v1/tasks/{target}/cancel"), json!({})).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(read_json(response).await["status"], "CANCELED");
        // This real router control is behind the cancel notification on the
        // same FIFO ingress. Its acknowledgement proves cancel was processed.
        let barrier = state
            .acp
            .call(
                "session/cancel_tool_call",
                json!({"sessionId":session_id,"toolCallId":"task-cancel-barrier-no-such-call"}),
            )
            .await
            .unwrap();
        assert_eq!(barrier["callId"], "task-cancel-barrier-no-such-call");
        assert_eq!(barrier["status"], "not_found");
        if matches!(steering, SteeringScenario::CancelQueued) {
            assert!(
                !first_cancelled.load(Ordering::SeqCst),
                "canceling queued B canceled A's actual runtime token"
            );
            assert_eq!(
                store.get_task(&first_id).unwrap().status,
                TaskStatus::AuthRequired,
                "canceling queued B interrupted running A"
            );
        } else {
            assert!(
                first_cancelled.load(Ordering::SeqCst),
                "current A cancellation did not reach its actual runtime token"
            );
            let (turn, _, second_cancelled) = runtime_bridges.recv().await.unwrap();
            assert_eq!(turn, 1);
            assert!(
                !second_cancelled.load(Ordering::SeqCst),
                "canceling A canceled B's actual queued runtime token"
            );
        }
    }
    if !matches!(steering, SteeringScenario::CancelRunning) {
        let reply = post(
            &app,
            &format!(
                "/v1/permission-requests/{}/respond",
                permission.payload["id"].as_str().unwrap()
            ),
            json!({"approved":true, "reviewer":"test"}),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::OK);
    }
    if matches!(steering, SteeringScenario::CancelQueued) {
        loop {
            let event = events.recv().await.unwrap();
            assert_ne!(event.event, "task.failed", "{}", event.payload);
            if event.event == "task.completed"
                && event.task_id.as_deref() == Some(first_id.as_str())
            {
                break;
            }
        }
        assert_eq!(
            store.get_task(&first_id).unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(
            store.get_task(&second_id).unwrap().status,
            TaskStatus::Canceled
        );
        assert_eq!(configurator.turns.load(Ordering::SeqCst), 1);
        assert!(!store
            .task_events(Some(&second_id), None)
            .unwrap()
            .iter()
            .any(|event| event.event == "permission.requested"));
        return;
    }
    let permission = next_permission(&mut events).await;
    assert_eq!(permission.task_id.as_deref(), Some(second_id.as_str()));
    assert_eq!(permission.payload["task_id"], second_id);
    assert_eq!(
        store.get_task(&second_id).unwrap().status,
        TaskStatus::AuthRequired
    );
    if matches!(steering, SteeringScenario::AcrossTurns) {
        let (turn, second_inputs, _) = runtime_bridges.recv().await.unwrap();
        assert_eq!(turn, 1);
        let pending = second_inputs.pending_injections_json().await;
        assert_eq!(
            pending["pendingCount"],
            0,
            "first task accepted={}; second task runtime queue={pending}",
            accepted_message.unwrap()
        );
    }
    let reply = post(
        &app,
        &format!(
            "/v1/permission-requests/{}/respond",
            permission.payload["id"].as_str().unwrap()
        ),
        json!({"approved":true, "reviewer":"test"}),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    // Some first-task terminal events were consumed while waiting for the
    // second approval. Its state read is durable; wait for the second terminal.
    loop {
        let event = events.recv().await.unwrap();
        assert_ne!(event.event, "task.failed", "{}", event.payload);
        if event.event == "task.completed" && event.task_id.as_deref() == Some(second_id.as_str()) {
            break;
        }
    }
    assert_eq!(
        store.get_task(&first_id).unwrap().status,
        if matches!(steering, SteeringScenario::CancelRunning) {
            TaskStatus::Canceled
        } else {
            TaskStatus::Completed
        }
    );
    assert_eq!(
        store.get_task(&second_id).unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(configurator.turns.load(Ordering::SeqCst), 2);
    for task_id in [&first_id, &second_id] {
        let audit = store.task_events(Some(task_id), None).unwrap();
        for name in ["permission.requested", "permission.responded"] {
            let events: Vec<_> = audit.iter().filter(|event| event.event == name).collect();
            if task_id == &first_id
                && name == "permission.responded"
                && matches!(steering, SteeringScenario::CancelRunning)
            {
                assert!(events.is_empty());
                continue;
            }
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].payload["task_id"], task_id.as_str());
        }
    }
}

#[tokio::test]
async fn queued_task_does_not_steal_running_raw_permission() {
    queued_task_does_not_steal_running_approval(true, SteeringScenario::None).await;
}

#[tokio::test]
async fn queued_task_does_not_steal_running_hitl_approval() {
    queued_task_does_not_steal_running_approval(false, SteeringScenario::None).await;
}

#[tokio::test]
async fn queued_task_input_cannot_reach_another_running_task() {
    queued_task_does_not_steal_running_approval(true, SteeringScenario::QueuedTask).await;
}

#[tokio::test]
async fn accepted_task_input_does_not_survive_into_another_task() {
    queued_task_does_not_steal_running_approval(true, SteeringScenario::AcrossTurns).await;
}

#[tokio::test]
async fn canceling_queued_task_preserves_running_task_approval() {
    queued_task_does_not_steal_running_approval(false, SteeringScenario::CancelQueued).await;
}

#[tokio::test]
async fn canceling_running_task_preserves_queued_task_execution() {
    queued_task_does_not_steal_running_approval(true, SteeringScenario::CancelRunning).await;
}
