use super::*;

#[tokio::test(flavor = "current_thread")]
async fn retry_exhaustion_moves_failed_dispatch_to_dlq() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  throw "boom"
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::new(2, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let event = trigger_event("issues.opened", "delivery-dlq");
            let outcomes = dispatcher
                .dispatch_event(event.clone())
                .await
                .expect("dispatch returns terminal outcome");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert_eq!(outcomes[0].attempt_count, 2);

            let dlq = dispatcher.dlq_entries();
            assert_eq!(dlq.len(), 1);
            assert_eq!(dlq[0].attempt_count, 2);
            assert_eq!(dlq[0].final_error, "boom");

            let dlq_topic = read_topic(log.clone(), "trigger.dlq").await;
            assert_eq!(dlq_topic.len(), 1);
            assert_eq!(dlq_topic[0].1.kind, "dlq_moved");

            let attempts = read_topic(log.clone(), "trigger.attempts").await;
            assert_eq!(
                attempts
                    .iter()
                    .filter(|(_, event)| event.kind == "attempt_recorded")
                    .count(),
                2
            );
            assert!(attempts
                .iter()
                .any(|(_, event)| event.kind == "retry_scheduled"));

            let graph = read_topic(log.clone(), "observability.action_graph").await;
            let (node_kinds, edge_kinds) = flatten_action_graph(&graph);
            assert!(node_kinds.iter().any(|kind| kind == "retry"));
            assert!(node_kinds.iter().any(|kind| kind == "dlq"));
            assert!(edge_kinds.iter().any(|kind| kind == "retry"));
            assert!(edge_kinds.iter().any(|kind| kind == "dlq_move"));
            assert!(graph.iter().any(|(_, event)| {
                event.payload["observability"]["action_graph_nodes"]
                    .as_array()
                    .is_some_and(|nodes| {
                        nodes.iter().any(|node| {
                            node["kind"] == serde_json::json!("dlq")
                                && node["metadata"]["attempt_count"] == serde_json::json!(2)
                                && node["metadata"]["final_error"] == serde_json::json!("boom")
                        })
                    })
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn destination_circuit_opens_and_dlqs_subsequent_dispatches() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  throw "provider 503"
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::new(7, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-circuit-open"))
                .await
                .expect("first dispatch returns terminal outcome");
            assert_eq!(first[0].status, DispatchStatus::Dlq);
            assert_eq!(first[0].attempt_count, 5);
            assert!(first[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("destination circuit opened")));

            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-circuit-fast-fail"))
                .await
                .expect("second dispatch returns terminal outcome");
            assert_eq!(second[0].status, DispatchStatus::Dlq);
            assert_eq!(second[0].attempt_count, 0);
            assert!(second[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("destination circuit open")));

            let dlq = dispatcher.dlq_entries();
            assert_eq!(dlq.len(), 2);
            assert_eq!(dlq[0].attempt_count, 5);
            assert_eq!(dlq[1].attempt_count, 0);

            let attempts = read_topic(log.clone(), "trigger.attempts").await;
            assert_eq!(
                attempts
                    .iter()
                    .filter(|(_, event)| event.kind == "attempt_recorded")
                    .count(),
                5
            );
            let dlq_topic = read_topic(log.clone(), "trigger.dlq").await;
            assert_eq!(dlq_topic.len(), 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn replay_dispatch_emits_replay_chain_edge_and_headers() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  return {kind: event.kind, id: event.id}
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let event = trigger_event("issues.opened", "delivery-replay");
            let outcome = dispatcher
                .dispatch_replay(&binding, event.clone(), "event-original".to_string())
                .await
                .expect("replay succeeds");

            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(
                outcome.replay_of_event_id.as_deref(),
                Some("event-original")
            );

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(outbox.iter().any(|(_, logged)| {
                logged.kind == "dispatch_succeeded"
                    && logged.headers.get("replay_of_event_id").map(String::as_str)
                        == Some("event-original")
            }));

            let graph = read_topic(log.clone(), "observability.action_graph").await;
            assert!(graph.iter().any(|(_, logged)| {
                logged.payload["observability"]["action_graph_edges"]
                    .as_array()
                    .is_some_and(|edges| {
                        edges.iter().any(|edge| {
                            edge.get("kind").and_then(|value| value.as_str())
                                == Some("replay_chain")
                                && edge.get("label").and_then(|value| value.as_str())
                                    == Some("replay chain")
                        })
                    })
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn replay_dispatch_scopes_harn_replay_per_dispatch_and_child_process() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let child_command = if cfg!(target_os = "windows") {
                "echo %HARN_REPLAY%"
            } else {
                "printf '%s' \"$HARN_REPLAY\""
            };
            let source = r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  let child = shell("__CHILD_COMMAND__")
  return {
    replay_env: env_or("HARN_REPLAY", "missing"),
    child_replay_env: child.stdout,
    dedupe_key: event.dedupe_key,
  }
}
"#
            .replace(
                "__CHILD_COMMAND__",
                &child_command.replace('\\', "\\\\").replace('"', "\\\""),
            );
            let (_dir, _log, dispatcher) =
                dispatcher_fixture(&source, "local_fn", None, TriggerRetryConfig::default()).await;

            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");

            let first_dispatcher = dispatcher.clone();
            let first_binding = binding.clone();
            let first = tokio::task::spawn_local(async move {
                first_dispatcher
                    .dispatch_replay(
                        &first_binding,
                        trigger_event("issues.opened", "delivery-env-a"),
                        "event-original-a".to_string(),
                    )
                    .await
                    .expect("first replay succeeds")
            });

            let second_dispatcher = dispatcher.clone();
            let second_binding = binding.clone();
            let second = tokio::task::spawn_local(async move {
                second_dispatcher
                    .dispatch_replay(
                        &second_binding,
                        trigger_event("issues.opened", "delivery-env-b"),
                        "event-original-b".to_string(),
                    )
                    .await
                    .expect("second replay succeeds")
            });

            let first = first.await.expect("join first replay");
            let second = second.await.expect("join second replay");

            let mut dedupe_keys = Vec::new();
            for outcome in [first, second] {
                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                let result = outcome.result.expect("replay result");
                assert_eq!(result["replay_env"], serde_json::json!("1"));
                assert_eq!(
                    result["child_replay_env"]
                        .as_str()
                        .expect("child replay env")
                        .trim(),
                    "1"
                );
                dedupe_keys.push(
                    result["dedupe_key"]
                        .as_str()
                        .expect("dedupe key")
                        .to_string(),
                );
            }
            dedupe_keys.sort();
            assert_eq!(dedupe_keys, vec!["delivery-env-a", "delivery-env-b"]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_propagates_cancel_to_all_in_flight_local_handlers() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn wait_for_cancel(event: TriggerEvent) -> string {
  while !is_cancelled() {
    sleep(1)
  }
  return event.kind
}
"#,
                "wait_for_cancel",
                None,
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let mut handles = Vec::new();
            for index in 0..3 {
                let dispatcher = dispatcher.clone();
                handles.push(tokio::task::spawn_local(async move {
                    dispatcher
                        .dispatch_event(trigger_event(
                            "issues.opened",
                            &format!("delivery-cancel-{index}"),
                        ))
                        .await
                        .expect("dispatch finishes")
                }));
            }

            wait_for_dispatcher_in_flight(&dispatcher, 3).await;
            dispatcher.shutdown();

            let mut cancelled = 0;
            for handle in handles {
                let outcomes = handle.await.expect("join local dispatch");
                assert_eq!(outcomes.len(), 1);
                if outcomes[0].status == DispatchStatus::Cancelled {
                    cancelled += 1;
                }
            }
            assert_eq!(cancelled, 3);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn external_cancel_request_cancels_in_flight_local_handler() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn wait_for_cancel(event: TriggerEvent) -> string {
  while !is_cancelled() {
    sleep(1)
  }
  return event.kind
}
"#,
                "wait_for_cancel",
                None,
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let binding = resolve_live_trigger_binding("github-new-issue", None)
                .expect("resolve binding for external cancel");
            let event = trigger_event("issues.opened", "delivery-external-cancel");
            let event_id = event.id.0.clone();
            let binding_key = binding.binding_key();

            let run_dispatcher = dispatcher.clone();
            let handle = tokio::task::spawn_local(async move {
                run_dispatcher
                    .dispatch(&binding, event)
                    .await
                    .expect("dispatch completes")
            });

            wait_for_dispatcher_in_flight(&dispatcher, 1).await;
            append_dispatch_cancel_request(
                &log,
                &DispatchCancelRequest {
                    binding_key: binding_key.clone(),
                    event_id: event_id.clone(),
                    requested_at: test_cancel_requested_at(),
                    requested_by: Some("test".to_string()),
                    audit_id: Some("audit-test".to_string()),
                },
            )
            .await
            .expect("append external cancel request");

            let outcome = handle.await.expect("join local dispatch");
            assert_eq!(outcome.status, DispatchStatus::Cancelled);
            assert!(
                outcome
                    .error
                    .as_deref()
                    .is_some_and(|message| message.contains("trigger cancel request")),
                "{outcome:?}"
            );

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(outbox.iter().any(|(_, event)| {
                event.kind == "dispatch_failed"
                    && event.headers.get("event_id").map(String::as_str) == Some(event_id.as_str())
                    && event
                        .payload
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|message| message.contains("trigger cancel request"))
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn external_cancel_request_interrupts_waitpoint_waiting_handler() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn wait_for_signal(event: TriggerEvent) -> string {
  waitpoint_create("cancel-demo")
  let result = waitpoint_wait("cancel-demo", {wait_id: "wait-cancel"})
  return result.status
}
"#,
                "wait_for_signal",
                None,
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let binding = resolve_live_trigger_binding("github-new-issue", None)
                .expect("resolve binding for external cancel");
            let event = trigger_event("issues.opened", "delivery-waitpoint-cancel");
            let event_id = event.id.0.clone();
            let binding_key = binding.binding_key();
            crate::waitpoints::clear_test_wait_signals();
            let (started_tx, started_rx) = oneshot::channel();
            crate::waitpoints::install_test_wait_signal(
                "wait-cancel",
                crate::waitpoints::WaitpointTestSignalKind::Started,
                started_tx,
            );
            let (interrupted_tx, interrupted_rx) = oneshot::channel();
            crate::waitpoints::install_test_wait_signal(
                "wait-cancel",
                crate::waitpoints::WaitpointTestSignalKind::Interrupted,
                interrupted_tx,
            );

            let run_dispatcher = dispatcher.clone();
            let handle = tokio::task::spawn_local(async move {
                run_dispatcher
                    .dispatch(&binding, event)
                    .await
                    .expect("dispatch completes")
            });

            await_test_signal("waitpoint_wait_started", started_rx).await;
            append_dispatch_cancel_request(
                &log,
                &DispatchCancelRequest {
                    binding_key: binding_key.clone(),
                    event_id: event_id.clone(),
                    requested_at: test_cancel_requested_at(),
                    requested_by: Some("test".to_string()),
                    audit_id: Some("audit-test".to_string()),
                },
            )
            .await
            .expect("append external cancel request");

            let outcome = handle.await.expect("join local dispatch");
            assert_eq!(outcome.status, DispatchStatus::Cancelled);
            assert!(
                outcome
                    .error
                    .as_deref()
                    .is_some_and(|message| message.contains("trigger cancel request")),
                "{outcome:?}"
            );

            await_test_signal("waitpoint_wait_interrupted", interrupted_rx).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_skips_historical_inbox_entries_on_startup() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.kind
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

            let historical = trigger_event("issues.opened", "delivery-historical");
            dispatcher
                .enqueue_targeted(
                    Some("github-new-issue".to_string()),
                    Some(1),
                    historical.clone(),
                )
                .await
                .expect("enqueue historical inbox entry");

            let dispatcher_for_task = dispatcher.clone();
            let run_task = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .run()
                    .await
                    .expect("dispatcher run exits");
            });

            tokio::time::sleep(NETWORK_PROBE_INITIAL).await;
            let outbox_before = read_topic(log.clone(), "trigger.outbox").await;
            assert!(
                outbox_before.is_empty(),
                "startup should not auto-dispatch historical inbox entries: {outbox_before:?}"
            );

            let live = trigger_event("issues.opened", "delivery-live");
            dispatcher
                .enqueue_targeted(Some("github-new-issue".to_string()), Some(1), live.clone())
                .await
                .expect("enqueue live inbox entry");

            let deadline = Instant::now() + TEST_DEFAULT_TIMEOUT;
            while Instant::now() < deadline {
                let outbox = read_topic(log.clone(), "trigger.outbox").await;
                if outbox.iter().any(|(_, event)| {
                    event.headers.get("event_id").map(String::as_str) == Some(live.id.0.as_str())
                        && event.kind == "dispatch_succeeded"
                }) {
                    break;
                }
                tokio::time::sleep(NETWORK_PROBE_INITIAL).await;
            }

            dispatcher.shutdown();
            run_task.await.expect("join dispatcher run task");

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(!outbox.iter().any(|(_, event)| {
                event.headers.get("event_id").map(String::as_str) == Some(historical.id.0.as_str())
            }));
            assert!(outbox.iter().any(|(_, event)| {
                event.headers.get("event_id").map(String::as_str) == Some(live.id.0.as_str())
                    && event.kind == "dispatch_succeeded"
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn drain_waits_for_in_flight_local_handlers_without_cancelling() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn slow_handler(event: TriggerEvent) -> string {
  sleep(50)
  return event.kind
}
"#,
                "slow_handler",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

            let dispatcher_for_task = dispatcher.clone();
            let handle = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-drain"))
                    .await
                    .expect("dispatch finishes")
            });

            wait_for_dispatcher_in_flight(&dispatcher, 1).await;
            let drain = dispatcher.drain(TEST_DEFAULT_TIMEOUT);
            tokio::pin!(drain);
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(50)).await;
            let report = drain.await.expect("drain completes");
            assert!(report.drained, "{report:?}");
            assert_eq!(report.in_flight, 0);
            assert_eq!(report.retry_queue_depth, 0);

            let outcomes = handle.await.expect("join local dispatch");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
        })
        .await;
}

// Regression coverage for harn#324: dispatcher shutdown must wake handlers
// that are blocked in `sleep()` so a cooperative `is_cancelled()` loop can
// exit without silently dropping an already-dequeued inbox event.
#[tokio::test(flavor = "current_thread")]
async fn run_shutdown_does_not_silently_drop_dequeued_inbox_events() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn wait_for_cancel(event: TriggerEvent) -> string {
  while !is_cancelled() {
    sleep(1)
  }
  return event.kind
}
"#,
                "wait_for_cancel",
                None,
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let (dequeued_tx, dequeued_rx) = oneshot::channel();
            super::install_test_inbox_dequeued_signal(dequeued_tx);

            let run_dispatcher = dispatcher.clone();
            let run_handle = tokio::task::spawn_local(async move {
                run_dispatcher.run().await.expect("dispatcher run exits cleanly");
            });

            tokio::task::yield_now().await;
            dispatcher
                .enqueue(trigger_event("issues.opened", "delivery-run-shutdown"))
                .await
                .expect("enqueue succeeds");

            tokio::time::timeout(TEST_DEFAULT_TIMEOUT, dequeued_rx)
                .await
                .expect("run should dequeue live inbox event")
                .expect("run dequeued inbox event");
            dispatcher.shutdown();
            run_handle.await.expect("join dispatcher run");
            let drain = dispatcher
                .drain(TEST_DEFAULT_TIMEOUT)
                .await
                .expect("shutdown drain completes");
            assert!(drain.drained, "{drain:?}");

            let inbox = read_topic(log.clone(), crate::TRIGGER_INBOX_ENVELOPES_TOPIC).await;
            assert_eq!(
                inbox.iter()
                    .filter(|(_, event)| event.kind == "event_ingested")
                .count(),
                1
            );
            let legacy_inbox = read_topic(log.clone(), "trigger.inbox").await;
            assert!(legacy_inbox.is_empty(), "legacy_inbox={legacy_inbox:?}");

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(
                outbox.iter().any(|(_, event)| event.kind == "dispatch_started"),
                "dequeued inbox event must either stay queued or emit an explicit outbox outcome"
            );
            assert!(
                outbox.iter().any(|(_, event)| event.kind == "dispatch_failed"),
                "shutdown-triggered cancellation must be recorded instead of silently dropping the inbox event"
            );
        })
        .await;
}
