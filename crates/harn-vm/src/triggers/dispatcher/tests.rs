// Replay tests serialize HARN_REPLAY env-var manipulation with a `Mutex<()>`
// so parallel test runs don't race. That guard is intentionally held across
// `.await` points while driving the dispatcher; silence clippy rather than
// introduce an async-aware mutex whose only purpose is to guard env state.
// Same pattern as `crates/harn-cli/tests/orchestrator_http.rs`.
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
use crate::register_vm_stdlib;
use crate::triggers::event::{GitHubEventPayload, KnownProviderPayload};
use crate::triggers::registry::{
    install_manifest_triggers, resolve_live_trigger_binding, TriggerBindingSource,
    TriggerBindingSpec, TriggerHandlerSpec, TriggerPredicateSpec,
};
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TraceId, TriggerEvent};
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

async fn a2a_dispatcher_fixture(
    target: String,
    retry: TriggerRetryConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());

    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-a2a-review".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        handler: TriggerHandlerSpec::A2a {
            target: target.clone(),
        },
        when: None,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        max_concurrent: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: format!("fp:{target}"),
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

struct MockA2aServer {
    authority: String,
    requests: Receiver<serde_json::Value>,
    join: thread::JoinHandle<()>,
}

impl MockA2aServer {
    fn next_request(&self) -> serde_json::Value {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("mock A2A request")
    }

    fn finish(self) {
        self.join.join().expect("mock A2A thread");
    }
}

fn spawn_mock_a2a_server(task_result: serde_json::Value) -> MockA2aServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock A2A listener");
    let addr = listener.local_addr().expect("mock A2A addr");
    let authority = format!("127.0.0.1:{}", addr.port());
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept mock A2A request");
            let (request_line, body) = read_http_request(&mut stream);
            if request_line.starts_with("GET /.well-known/a2a-agent ") {
                write_json_response(
                    &mut stream,
                    &serde_json::json!({
                        "id": "mock-a2a",
                        "url": format!("http://127.0.0.1:{}", addr.port()),
                        "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
                    }),
                );
                continue;
            }
            assert!(
                request_line.starts_with("POST /rpc "),
                "unexpected request line: {request_line}"
            );
            tx.send(
                serde_json::from_slice::<serde_json::Value>(&body).expect("mock A2A request json"),
            )
            .expect("capture mock A2A request");
            let rpc_id = serde_json::from_slice::<serde_json::Value>(&body)
                .expect("mock A2A request json")["id"]
                .clone();
            write_json_response(
                &mut stream,
                &crate::jsonrpc::response(rpc_id, task_result.clone()),
            );
        }
    });
    MockA2aServer {
        authority,
        requests: rx,
        join,
    }
}

fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end;
    let content_length;
    loop {
        let read = stream.read(&mut chunk).expect("read mock A2A request");
        assert!(read > 0, "mock A2A request closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&buffer) {
            header_end = end;
            content_length = parse_content_length(&buffer[..header_end]);
            break;
        }
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).expect("read mock A2A body");
        assert!(read > 0, "mock A2A request closed before body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = headers.lines().next().unwrap_or_default().to_string();
    let body = buffer[header_end..header_end + content_length].to_vec();
    (request_line, body)
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn write_json_response(stream: &mut TcpStream, body: &serde_json::Value) {
    let payload = serde_json::to_vec(body).expect("serialize mock A2A response");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write mock A2A headers");
    stream.write_all(&payload).expect("write mock A2A body");
    stream.flush().expect("flush mock A2A response");
}

fn replay_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
            assert_eq!(request["method"], "a2a.SendMessage");
            let envelope_text = request["params"]["message"]["parts"][0]["text"]
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
                    "rpc_url": format!("http://{}/rpc", server.authority),
                    "card_url": format!("http://{}/.well-known/a2a-agent", server.authority),
                    "agent_id": "mock-a2a",
                }))
            );

            let request = server.next_request();
            assert_eq!(request["method"], "a2a.SendMessage");
            server.finish();
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
async fn replay_dispatch_sets_harn_replay_env_and_restores_previous_value() {
    let _env_guard = replay_env_lock().lock().expect("env lock poisoned");
    std::env::set_var("HARN_REPLAY", "outer");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return env_or("HARN_REPLAY", "missing")
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let outcome = dispatcher
                .dispatch_replay(
                    &binding,
                    trigger_event("issues.opened", "delivery-env"),
                    "event-original".to_string(),
                )
                .await
                .expect("replay succeeds");

            assert_eq!(outcome.result, Some(serde_json::json!("1")));
            assert_eq!(std::env::var("HARN_REPLAY").ok().as_deref(), Some("outer"));
        })
        .await;

    std::env::remove_var("HARN_REPLAY");
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
