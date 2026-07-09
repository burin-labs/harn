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
async fn enqueue_writes_raw_dispatch_and_redacted_observability_records() {
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

            let mut event = trigger_event("issues.opened", "delivery-observed");
            event.headers.insert(
                "authorization".to_string(),
                "Bearer raw-header-secret".to_string(),
            );
            event.raw_body = Some(br#"{"note":"raw-body-secret"}"#.to_vec());
            if let ProviderPayload::Known(KnownProviderPayload::GitHub(payload)) =
                &mut event.provider_payload
            {
                if let GitHubEventPayload::Issues(payload) = payload.as_mut() {
                    payload.common.raw = serde_json::json!({"neutral": "raw-provider-secret"});
                    payload.issue = serde_json::json!({"title": "raw-provider-secret"});
                }
            }

            dispatcher
                .enqueue_targeted(Some("github-new-issue".to_string()), Some(1), event)
                .await
                .expect("enqueue succeeds");

            let raw = read_topic(log.clone(), crate::TRIGGER_INBOX_ENVELOPES_TOPIC).await;
            let observed = read_topic(log.clone(), crate::TRIGGER_INBOX_OBSERVABILITY_TOPIC).await;
            assert_eq!(raw.len(), 1);
            assert_eq!(observed.len(), 1);

            let raw_text = serde_json::to_string(&raw[0].1).unwrap();
            assert!(raw_text.contains("raw-header-secret"));
            assert!(raw_text.contains("raw-provider-secret"));

            let observed_event = &observed[0].1;
            assert_eq!(observed_event.kind, "event_ingested");
            assert_eq!(
                observed_event.payload["schema_version"],
                serde_json::json!(1)
            );
            assert_eq!(
                observed_event.payload["event"]["raw_body"]["byte_len"],
                serde_json::json!(26)
            );
            assert!(observed_event.payload["event"]["raw_body"]["sha256"]
                .as_str()
                .is_some_and(|digest| digest.len() == 64));
            assert!(observed_event.payload["event"]
                .get("provider_payload")
                .is_none());

            let observed_text = serde_json::to_string(observed_event).unwrap();
            assert!(observed_text.contains("[redacted]"));
            for secret in [
                "raw-header-secret",
                "raw-body-secret",
                "raw-provider-secret",
            ] {
                assert!(
                    !observed_text.contains(secret),
                    "observability event leaked {secret}: {observed_text}"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn eval_pack_handler_runs_from_cron_tick_and_sheds_after_budget() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_test_event_log();
            let manifest = crate::triggers::test_util::scheduled_eval_manifest(dir.path())
                .expect("scheduled eval manifest normalizes");

            let mut vm = Vm::new();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            install_manifest_triggers(vec![TriggerBindingSpec {
                id: "nightly-eval".to_string(),
                source: TriggerBindingSource::Manifest,
                kind: "cron".to_string(),
                provider: ProviderId::from("cron"),
                autonomy_tier: crate::AutonomyTier::Suggest,
                handler: TriggerHandlerSpec::EvalPack {
                    target: "scheduled-eval".to_string(),
                    manifest: Box::new(manifest),
                    ledger_options: None,
                },
                dispatch_priority: crate::WorkerQueuePriority::Normal,
                when: None,
                when_budget: None,
                retry: TriggerRetryConfig::default(),
                match_events: vec!["cron.tick".to_string()],
                dedupe_key: None,
                dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
                filter: None,
                daily_cost_usd: Some(0.10),
                hourly_cost_usd: None,
                max_autonomous_decisions_per_hour: None,
                max_autonomous_decisions_per_day: None,
                on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
                max_concurrent: None,
                flow_control: crate::triggers::TriggerFlowControlConfig {
                    concurrency: Some(crate::triggers::TriggerConcurrencyConfig {
                        key: None,
                        max: 1,
                    }),
                    ..Default::default()
                },
                aggregation: None,
                manifest_path: None,
                package_name: Some("workspace".to_string()),
                definition_fingerprint: "fp:scheduled-eval".to_string(),
            }])
            .await
            .expect("install eval trigger");

            let dispatcher = Dispatcher::with_event_log(vm, log.clone());
            let first = dispatcher
                .dispatch_event(crate::triggers::test_util::scheduled_eval_cron_tick(
                    "nightly-eval",
                    "tick-1",
                ))
                .await
                .expect("first tick dispatches");

            assert_eq!(first.len(), 1);
            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            let report = first[0].result.as_ref().expect("eval report");
            assert_eq!(report["pack_id"], serde_json::json!("scheduled-eval"));
            assert_eq!(report["pass"], serde_json::json!(true));
            assert_eq!(
                report["run_state"]["ledger_rows_inserted"],
                serde_json::json!(1)
            );

            let ledger = crate::orchestration::eval_ledger_read_report(Some(serde_json::json!({
                "namespace": "scheduled-eval",
            })))
            .expect("ledger read");
            assert_eq!(ledger.rows.len(), 1);
            assert_eq!(ledger.rows[0].suite, "scheduled-eval");
            assert_eq!(ledger.rows[0].provenance.commit, "commit-a");

            let second = dispatcher
                .dispatch_event(crate::triggers::test_util::scheduled_eval_cron_tick(
                    "nightly-eval",
                    "tick-2",
                ))
                .await
                .expect("second tick sheds");
            assert_eq!(second.len(), 1);
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "budget": "daily_budget_exceeded",
                }))
            );

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            let skipped = outbox
                .iter()
                .find(|(_, event)| event.kind == "dispatch_skipped")
                .expect("budget-shed tick emits skipped outbox record");
            assert_eq!(skipped.1.payload["skip_stage"], serde_json::json!("budget"));
            assert_eq!(
                skipped.1.payload["detail"]["budget"],
                serde_json::json!("daily_budget_exceeded")
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
            assert!(graph.iter().any(|(_, logged)| {
                logged.payload["observability"]["action_graph_nodes"]
                    .as_array()
                    .is_some_and(|nodes| {
                        nodes.iter().any(|node| {
                            node["kind"] == serde_json::json!("a2a_hop")
                                && node["metadata"]["trust_boundary"]
                                    == serde_json::json!("federated_a2a")
                                && node["metadata"]["execution_location"]
                                    == serde_json::json!("remote")
                                && node["metadata"]["remote_identity"]
                                    == serde_json::json!("triage")
                                && node["metadata"]["remote_agent_id"]
                                    == serde_json::json!("agent:mock-a2a/triage")
                        })
                    })
            }));

            let trust_records = crate::query_trust_records(
                &log,
                &crate::TrustQueryFilters {
                    agent: Some("github-a2a-review".to_string()),
                    action: Some("github.issues.opened".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("query trust records");
            assert!(trust_records.iter().any(|logged| {
                logged.metadata["trust_boundary"] == serde_json::json!("federated_a2a")
                    && logged.metadata["remote_identity"] == serde_json::json!("triage")
            }));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn local_and_a2a_handlers_preserve_logical_output_and_replay_shape() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let expected = serde_json::json!({
                "status": "triaged",
                "labels": ["needs-owner"],
            });
            let (_local_dir, local_log, local_dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(_event: TriggerEvent) -> dict {
  return {status: "triaged", labels: ["needs-owner"]}
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;
            let mut local_event = trigger_event("issues.opened", "delivery-local-equivalent");
            local_event.trace_id = TraceId("trace-equivalent".to_string());
            let local_outcome = local_dispatcher
                .dispatch_event(local_event)
                .await
                .expect("local dispatch succeeds")
                .pop()
                .expect("local outcome");
            let local_outbox = read_topic(local_log.clone(), "trigger.outbox").await;
            let local_attempts = read_topic(local_log.clone(), "trigger.attempts").await;

            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Inline {
                task_id: "task-equivalent".to_string(),
                result: expected.clone(),
            });
            let (_a2a_dir, a2a_log, a2a_dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::default(),
                false,
                mock_client,
            )
            .await;
            let mut a2a_event = trigger_event("issues.opened", "delivery-a2a-equivalent");
            a2a_event.trace_id = TraceId("trace-equivalent".to_string());
            let a2a_outcome = a2a_dispatcher
                .dispatch_event(a2a_event)
                .await
                .expect("A2A dispatch succeeds")
                .pop()
                .expect("A2A outcome");
            let a2a_outbox = read_topic(a2a_log.clone(), "trigger.outbox").await;
            let a2a_attempts = read_topic(a2a_log.clone(), "trigger.attempts").await;

            assert_eq!(local_outcome.result, Some(expected.clone()));
            assert_eq!(a2a_outcome.result, Some(expected));
            assert_eq!(
                local_outbox
                    .iter()
                    .map(|(_, event)| event.kind.as_str())
                    .collect::<Vec<_>>(),
                a2a_outbox
                    .iter()
                    .map(|(_, event)| event.kind.as_str())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(
                local_attempts
                    .iter()
                    .map(|(_, event)| event.kind.as_str())
                    .collect::<Vec<_>>(),
                a2a_attempts
                    .iter()
                    .map(|(_, event)| event.kind.as_str())
                    .collect::<Vec<_>>(),
            );

            let local_success = local_outbox
                .iter()
                .find(|(_, event)| event.kind == "dispatch_succeeded")
                .expect("local dispatch_succeeded");
            let a2a_success = a2a_outbox
                .iter()
                .find(|(_, event)| event.kind == "dispatch_succeeded")
                .expect("A2A dispatch_succeeded");
            assert_eq!(
                local_success.1.payload["dispatch_metadata"]["trust_boundary"],
                serde_json::json!("local_process")
            );
            assert_eq!(
                a2a_success.1.payload["dispatch_metadata"]["trust_boundary"],
                serde_json::json!("federated_a2a")
            );
            assert_eq!(
                a2a_success.1.payload["dispatch_metadata"]["remote_identity"],
                serde_json::json!("triage")
            );
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
    // Use `i64::MAX` as the "now" reference so every enqueued job is past its
    // scheduled `not_before` and counted as ready, independent of wall-clock
    // drift between fixture setup and assertion.
    assert_eq!(state.summary(i64::MAX).ready, 1);
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
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Denied(
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
            assert_eq!(outcomes[0].status, DispatchStatus::Failed);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("allow_cleartext = true")));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_timeout_retries_then_dlqs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Timeout(
                "A2A HTTP request timed out".to_string(),
            ));
            let (_dir, log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                false,
                mock_client,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-timeout"))
                .await
                .expect("timeout returns terminal outcome");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("timed out")));

            let attempts = read_topic(log.clone(), "trigger.attempts").await;
            assert!(attempts.iter().any(|(_, event)| {
                event.kind == "attempt_recorded"
                    && event.payload["outcome"] == serde_json::json!("timeout")
            }));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_incompatible_schema_dlqs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Protocol(
                "A2A task response missing result.status.state".to_string(),
            ));
            let (_dir, _log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                false,
                mock_client,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-schema"))
                .await
                .expect("schema mismatch returns terminal outcome");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("result.status.state")));
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a2a_permission_rejection_fails_closed_without_retry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mock_client = InProcessMockA2aClient::new(MockA2aResponse::Denied(
                "A2A task rejected by remote agent: permission denied".to_string(),
            ));
            let (_dir, log, dispatcher) = a2a_dispatcher_fixture(
                "mock-a2a/triage".to_string(),
                TriggerRetryConfig::new(3, RetryPolicy::Linear { delay_ms: 0 }),
                false,
                mock_client.clone(),
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-a2a-denied"))
                .await
                .expect("permission rejection returns terminal outcome");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Failed);
            assert_eq!(outcomes[0].attempt_count, 1);
            assert!(outcomes[0]
                .error
                .as_deref()
                .is_some_and(|message| message.contains("permission denied")));
            assert_eq!(mock_client.take_calls().await.len(), 1);

            let trust_records = crate::query_trust_records(
                &log,
                &crate::TrustQueryFilters {
                    agent: Some("github-a2a-review".to_string()),
                    action: Some("github.issues.opened".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("query trust records");
            assert!(trust_records.iter().any(|event| {
                event.outcome == crate::TrustOutcome::Denied
                    && event.metadata["terminal_status"] == serde_json::json!("failed")
                    && event.metadata["trust_boundary"] == serde_json::json!("federated_a2a")
            }));
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
