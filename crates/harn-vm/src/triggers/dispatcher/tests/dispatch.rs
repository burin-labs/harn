use super::*;

#[tokio::test(flavor = "current_thread")]
async fn local_handler_round_trip_logs_outbox_lifecycle_and_action_graph() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  return event.kind == "issues.opened"
}
"#,
                "local_fn",
                Some("should_handle"),
                TriggerRetryConfig::default(),
            )
            .await;

            let event = trigger_event("issues.opened", "delivery-roundtrip");
            let outcomes = dispatcher
                .dispatch_event(event.clone())
                .await
                .expect("dispatch succeeds");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(outcomes[0].result, Some(serde_json::json!("issues.opened")));

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(outbox
                .iter()
                .any(|(_, event)| event.kind == "dispatch_started"));
            assert!(outbox.iter().any(|(_, event)| {
                event.kind == "dispatch_succeeded"
                    && event.payload["result"] == serde_json::json!("issues.opened")
            }));

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            assert!(lifecycle
                .iter()
                .any(|(_, event)| event.kind == "DispatchStarted"));
            assert!(lifecycle
                .iter()
                .any(|(_, event)| event.kind == "DispatchSucceeded"));

            let graph = read_topic(log.clone(), "observability.action_graph").await;
            let (node_kinds, edge_kinds) = flatten_action_graph(&graph);
            assert!(node_kinds.iter().any(|kind| kind == "trigger"));
            assert!(node_kinds.iter().any(|kind| kind == "predicate"));
            assert!(node_kinds.iter().any(|kind| kind == "dispatch"));
            assert!(edge_kinds.iter().any(|kind| kind == "trigger_dispatch"));
            assert!(edge_kinds.iter().any(|kind| kind == "predicate_gate"));
            assert!(graph.iter().any(|(_, event)| {
                event.payload["observability"]["action_graph_nodes"]
                    .as_array()
                    .is_some_and(|nodes| {
                        nodes.iter().any(|node| {
                            node["kind"] == serde_json::json!("dispatch")
                                && node["status"] == serde_json::json!("completed")
                                && node["metadata"]["handler_kind"] == serde_json::json!("local")
                        })
                    })
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_handler_receives_raw_body_as_bytes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  return {
    raw_body_type: type_of(event.raw_body),
    raw_body_text: bytes_to_string(event.raw_body ?? bytes_from_string("")),
  }
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

            let mut event = trigger_event("issues.opened", "delivery-raw-body");
            event.raw_body = Some(b"Hello, World!".to_vec());

            let outcomes = dispatcher
                .dispatch_event(event)
                .await
                .expect("dispatch succeeds");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                outcomes[0].result,
                Some(serde_json::json!({
                    "raw_body_type": "bytes",
                    "raw_body_text": "Hello, World!",
                }))
            );
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_handler_returns_inline_result_and_emits_a2a_action_graph() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Inline {
                task_id: "task-inline".to_string(),
                result: serde_json::json!({"trace_id": "trace_inline", "target_agent": "triage"}),
            });
            let (_dir, log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::default(),
                false,
                mock_client.clone(),
            )
            .await;

            let mut event = trigger_event("issues.opened", "delivery-a2a-inline");
            event.trace_id = TraceId("trace_inline".to_string());

            let outcomes = dispatcher
                .dispatch_event(event.clone())
                .await
                .expect("A2A dispatch succeeds");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                outcomes[0].result,
                Some(serde_json::json!({
                    "trace_id": "trace_inline",
                    "target_agent": "triage",
                }))
            );

            let calls = mock_client.take_calls().await;
            assert_eq!(calls.len(), 1);
            assert!(calls[0].target.ends_with("/triage"));
            assert_eq!(calls[0].event_trace_id, "trace_inline");

            let graph = read_topic(log.clone(), "observability.action_graph").await;
            let (node_kinds, edge_kinds) = flatten_action_graph(&graph);
            assert!(node_kinds.iter().any(|kind| kind == "a2a_hop"));
            assert!(edge_kinds.iter().any(|kind| kind == "a2a_dispatch"));
            assert!(graph.iter().any(|(_, logged)| {
                logged.headers.get("trace_id").map(String::as_str) == Some("trace_inline")
                    && logged.payload["context"]["target_agent"] == serde_json::json!("triage")
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn worker_handler_enqueues_job_and_returns_receipt() {
    let (_dir, log, dispatcher) = worker_dispatcher_fixture(
        "triage".to_string(),
        TriggerRetryConfig::default(),
        crate::WorkerQueuePriority::High,
    )
    .await;

    let outcomes = dispatcher
        .dispatch_event(trigger_event("issues.opened", "delivery-worker"))
        .await
        .expect("worker dispatch succeeds");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
    assert_eq!(outcomes[0].handler_kind, "worker");
    assert_eq!(outcomes[0].target_uri, "worker://triage");

    let receipt = outcomes[0]
        .result
        .clone()
        .expect("worker dispatch returns enqueue receipt");
    assert_eq!(receipt["queue"], serde_json::json!("triage"));
    assert_eq!(
        receipt["response_topic"],
        serde_json::json!(crate::worker_response_topic_name("triage"))
    );
    assert!(receipt["job_event_id"].as_u64().is_some());

    let queue = crate::WorkerQueue::new(log.clone());
    let state = queue.queue_state("triage").await.expect("load queue state");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64;
    assert_eq!(state.summary(now_ms).ready, 1);
    assert_eq!(state.jobs.len(), 1);
    assert_eq!(state.jobs[0].job.trigger_id, "github-worker-review");
    assert_eq!(state.jobs[0].job.priority, crate::WorkerQueuePriority::High);

    let graph = read_topic(log.clone(), "observability.action_graph").await;
    assert!(graph.iter().any(|(_, event)| {
        event.payload["observability"]["action_graph_nodes"]
            .as_array()
            .is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node["kind"] == serde_json::json!("worker_enqueue")
                        && node["metadata"]["queue_name"] == serde_json::json!("triage")
                        && node["metadata"]["job_event_id"].as_u64().is_some()
                })
            })
    }));
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_handler_returns_pending_task_handle() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let pending_handle = serde_json::json!({
                "kind": "a2a_task_handle",
                "task_id": "task-pending",
                "state": "working",
                "target_agent": "triage",
                "rpc_url": "https://mock-a2a/rpc",
                "card_url": "https://mock-a2a/.well-known/agent-card.json",
                "agent_id": null,
            });
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Pending {
                task_id: "task-pending".to_string(),
                state: "working".to_string(),
                handle: pending_handle.clone(),
            });
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::default(),
                false,
                mock_client.clone(),
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-pending"))
                .await
                .expect("A2A dispatch returns pending handle");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(outcomes[0].result, Some(pending_handle));

            let calls = mock_client.take_calls().await;
            assert_eq!(calls.len(), 1);
            assert!(calls[0].target.ends_with("/triage"));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn shutdown_cancels_a2a_dispatch_started_after_shutdown() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // WaitForCancel makes the mock block on the cancel receiver.
            // In practice the shutting_down pre-check fires first under
            // cooperative scheduling, so the mock is never reached — both
            // behaviours correctly produce a Cancelled outcome.
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::WaitForCancel);
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::default(),
                false,
                mock_client.clone(),
            )
            .await;

            let dispatcher_for_task = dispatcher.clone();
            let handle = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-a2a-shutdown"))
                    .await
                    .expect("dispatch finishes")
            });

            dispatcher.shutdown();

            let outcomes = handle.await.expect("join A2A dispatch");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Cancelled);
            assert_eq!(outcomes[0].result, None);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("cancelled")));
            assert!(
                mock_client.take_calls().await.is_empty(),
                "A2A dispatch should not reach the remote after shutdown"
            );
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_handler_rejects_cleartext_by_default() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Protocol(
                "A2A endpoint uses cleartext HTTP; set allow_cleartext = true to opt in"
                    .to_string(),
            ));
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                false,
                mock_client.clone(),
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-http-denied"))
                .await
                .expect("cleartext denial returns terminal outcome");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("allow_cleartext = true")));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_handler_allows_cleartext_after_opt_in() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Inline {
                task_id: "task-inline".to_string(),
                result: serde_json::json!({"trace_id": "trace_http", "target_agent": "triage"}),
            });
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::default(),
                true,
                mock_client.clone(),
            )
            .await;

            let mut event = trigger_event("issues.opened", "delivery-a2a-http-allowed");
            event.trace_id = TraceId("trace_http".to_string());

            let outcomes = dispatcher
                .dispatch_event(event)
                .await
                .expect("cleartext A2A dispatch succeeds after opt-in");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                outcomes[0].result,
                Some(serde_json::json!({
                    "trace_id": "trace_http",
                    "target_agent": "triage",
                }))
            );

            let calls = mock_client.take_calls().await;
            assert_eq!(calls.len(), 1);
            assert!(
                calls[0].allow_cleartext,
                "cleartext flag must be passed through"
            );
            assert_eq!(calls[0].event_trace_id, "trace_http");
        })
        .await;
}
