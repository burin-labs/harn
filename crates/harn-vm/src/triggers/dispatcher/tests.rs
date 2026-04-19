use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
use crate::register_vm_stdlib;
use crate::triggers::event::{GitHubEventPayload, KnownProviderPayload};
use crate::triggers::registry::{
    install_manifest_triggers, resolve_live_trigger_binding, TriggerBindingSource,
    TriggerBindingSpec, TriggerHandlerSpec, TriggerPredicateSpec,
};
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TriggerEvent};
use crate::Vm;

use super::retry::TriggerRetryConfig;
use super::uri::{DispatchUri, DispatchUriError};
use super::{DispatchStatus, Dispatcher, RetryPolicy};

fn trigger_event(kind: &str, dedupe_key: &str) -> TriggerEvent {
    TriggerEvent::new(
        ProviderId::from("github"),
        kind,
        None,
        dedupe_key,
        None,
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::GitHub(GitHubEventPayload::Issues(
            crate::triggers::event::GitHubIssuesEventPayload {
                common: crate::triggers::event::GitHubEventCommon {
                    event: "issues".to_string(),
                    action: Some("opened".to_string()),
                    delivery_id: Some(dedupe_key.to_string()),
                    installation_id: Some(42),
                    raw: serde_json::json!({"action":"opened"}),
                },
                issue: serde_json::json!({}),
            },
        ))),
        SignatureStatus::Verified,
    )
}

async fn dispatcher_fixture(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    retry: TriggerRetryConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let lib_path = dir.path().join("lib.harn");
    std::fs::write(&lib_path, source).expect("write module source");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let exports = vm
        .load_module_exports(&lib_path)
        .await
        .expect("load handler exports");

    let handler = exports
        .get(handler_name)
        .unwrap_or_else(|| panic!("missing handler export {handler_name}"))
        .clone();
    let when = when_name.map(|name| TriggerPredicateSpec {
        raw: name.to_string(),
        closure: exports
            .get(name)
            .unwrap_or_else(|| panic!("missing predicate export {name}"))
            .clone(),
    });

    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-new-issue".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        handler: TriggerHandlerSpec::Local {
            raw: handler_name.to_string(),
            closure: handler,
        },
        when,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        max_concurrent: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: format!("fp:{handler_name}"),
    }])
    .await
    .expect("install test trigger binding");

    (dir, log.clone(), Dispatcher::with_event_log(vm, log))
}

async fn read_topic(
    log: Arc<crate::event_log::AnyEventLog>,
    topic: &str,
) -> Vec<(u64, crate::event_log::LogEvent)> {
    let topic = Topic::new(topic).expect("valid topic");
    log.read_range(&topic, None, usize::MAX)
        .await
        .expect("read topic events")
}

fn flatten_action_graph(
    events: &[(u64, crate::event_log::LogEvent)],
) -> (Vec<String>, Vec<String>) {
    let mut node_kinds = Vec::new();
    let mut edge_kinds = Vec::new();
    for (_, event) in events {
        let observability = &event.payload["observability"];
        if let Some(nodes) = observability["action_graph_nodes"].as_array() {
            node_kinds.extend(nodes.iter().filter_map(|node| {
                node.get("kind")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }));
        }
        if let Some(edges) = observability["action_graph_edges"].as_array() {
            edge_kinds.extend(edges.iter().filter_map(|edge| {
                edge.get("kind")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }));
        }
    }
    (node_kinds, edge_kinds)
}

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
        })
        .await;
}

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

            let start = Instant::now();
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

            tokio::time::sleep(Duration::from_millis(10)).await;
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
            assert!(
                start.elapsed() <= Duration::from_millis(100),
                "all in-flight dispatches must observe cancellation within 100ms"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
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

            tokio::time::sleep(Duration::from_millis(10)).await;
            let report = dispatcher
                .drain(Duration::from_secs(1))
                .await
                .expect("drain completes");
            assert!(report.drained, "{report:?}");
            assert_eq!(report.in_flight, 0);
            assert_eq!(report.retry_queue_depth, 0);

            let outcomes = handle.await.expect("join local dispatch");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
        })
        .await;
}

#[test]
fn uri_parser_rejects_invalid_and_unknown_handler_schemes() {
    assert_eq!(DispatchUri::parse("").unwrap_err(), DispatchUriError::Empty);
    assert_eq!(
        DispatchUri::parse("a2a://").unwrap_err(),
        DispatchUriError::MissingTarget {
            scheme: "a2a".to_string()
        }
    );
    assert_eq!(
        DispatchUri::parse("worker://").unwrap_err(),
        DispatchUriError::MissingTarget {
            scheme: "worker".to_string()
        }
    );
    assert_eq!(
        DispatchUri::parse("smtp://relay").unwrap_err(),
        DispatchUriError::UnknownScheme("smtp".to_string())
    );
}
