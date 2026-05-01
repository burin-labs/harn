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

#[tokio::test(flavor = "current_thread")]
async fn a2a_handler_returns_inline_result_and_emits_a2a_action_graph() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = spawn_mock_a2a_server(serde_json::json!({
                "id": "task-inline",
                "status": {"state": "completed"},
                "history": [
                    {"id": "msg-user", "role": "user", "parts": [{"type": "text", "text": "ignored"}]},
                    {"id": "msg-agent", "role": "agent", "parts": [{"type": "text", "text": "{\"trace_id\":\"trace_inline\",\"target_agent\":\"triage\"}"}]},
                ],
                "artifacts": [],
            }));
            let (_dir, log, dispatcher) = a2a_dispatcher_fixture(
                format!("{}/triage", server.authority),
                TriggerRetryConfig::default(),
                false,
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

            let request = server.next_request();
            assert_eq!(request.body["method"], "message/send");
            assert_eq!(
                request.headers.get("a2a-trace-id").map(String::as_str),
                Some("trace_inline")
            );
            let envelope_text = request.body["params"]["message"]["parts"][0]["text"]
                .as_str()
                .expect("A2A text part");
            let envelope: serde_json::Value =
                serde_json::from_str(envelope_text).expect("A2A envelope JSON");
            assert_eq!(envelope["trace_id"], "trace_inline");
            assert_eq!(envelope["target_agent"], "triage");
            assert_eq!(envelope["event"]["trace_id"], "trace_inline");

            let graph = read_topic(log.clone(), "observability.action_graph").await;
            let (node_kinds, edge_kinds) = flatten_action_graph(&graph);
            assert!(node_kinds.iter().any(|kind| kind == "a2a_hop"));
            assert!(edge_kinds.iter().any(|kind| kind == "a2a_dispatch"));
            assert!(graph.iter().any(|(_, logged)| {
                logged.headers.get("trace_id").map(String::as_str) == Some("trace_inline")
                    && logged.payload["context"]["target_agent"] == serde_json::json!("triage")
            }));

            server.finish();
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

#[tokio::test(flavor = "current_thread")]
async fn a2a_handler_returns_pending_task_handle() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = spawn_mock_a2a_server(serde_json::json!({
                "id": "task-pending",
                "status": {"state": "working"},
                "history": [
                    {"id": "msg-user", "role": "user", "parts": [{"type": "text", "text": "ignored"}]},
                ],
                "artifacts": [],
            }));
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                format!("{}/triage", server.authority),
                TriggerRetryConfig::default(),
                false,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-pending"))
                .await
                .expect("A2A dispatch returns pending handle");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                outcomes[0].result,
                Some(serde_json::json!({
                    "kind": "a2a_task_handle",
                    "task_id": "task-pending",
                    "state": "working",
                    "target_agent": "triage",
                    "rpc_url": format!("https://{}/rpc", server.authority),
                    "card_url": format!("https://{}/.well-known/agent-card.json", server.authority),
                    "agent_id": null,
                }))
            );

            let request = server.next_request();
            assert_eq!(request.body["method"], "message/send");
            server.finish();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancels_a2a_dispatch_started_after_shutdown() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = spawn_mock_a2a_server(serde_json::json!({
                "id": "task-inline",
                "status": {"state": "completed"},
                "history": [
                    {"id": "msg-user", "role": "user", "parts": [{"type": "text", "text": "ignored"}]},
                    {"id": "msg-agent", "role": "agent", "parts": [{"type": "text", "text": "\"unexpected\""}]},
                ],
                "artifacts": [],
            }));
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                format!("{}/triage", server.authority),
                TriggerRetryConfig::default(),
                false,
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
                server.request_within(PROCESS_EXIT_GRACE).is_none(),
                "A2A dispatch should not reach the remote after shutdown"
            );

            server.finish();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a2a_handler_rejects_cleartext_by_default() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = spawn_mock_https_a2a_server_with_card_scheme(serde_json::json!({
                "id": "task-inline",
                "status": {"state": "completed"},
                "history": [
                    {"id": "msg-agent", "role": "agent", "parts": [{"type": "text", "text": "\"unexpected\""}]},
                ],
                "artifacts": [],
            }), "http");
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                format!("{}/triage", server.authority),
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                false,
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
            assert!(
                server.request_within(PROCESS_EXIT_GRACE).is_none(),
                "cleartext A2A dispatch should not reach the HTTP rpc endpoint without opt-in"
            );

            server.finish();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a2a_handler_allows_cleartext_after_opt_in() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = spawn_mock_http_a2a_server(serde_json::json!({
                "id": "task-inline",
                "status": {"state": "completed"},
                "history": [
                    {"id": "msg-user", "role": "user", "parts": [{"type": "text", "text": "ignored"}]},
                    {"id": "msg-agent", "role": "agent", "parts": [{"type": "text", "text": "{\"trace_id\":\"trace_http\",\"target_agent\":\"triage\"}"}]},
                ],
                "artifacts": [],
            }));
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                format!("{}/triage", server.authority),
                TriggerRetryConfig::default(),
                true,
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

            let request = server.next_request();
            assert_eq!(request.body["method"], "message/send");
            assert_eq!(
                request.headers.get("a2a-trace-id").map(String::as_str),
                Some("trace_http")
            );

            server.finish();
        })
        .await;
}
