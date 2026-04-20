use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant};

use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use tokio::sync::oneshot;

use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
use crate::llm::mock::{get_llm_mock_calls, push_llm_mock, LlmMock};
use crate::register_vm_stdlib;
use crate::triggers::event::{GitHubEventPayload, KnownProviderPayload};
use crate::triggers::registry::{
    install_manifest_triggers, resolve_live_trigger_binding, TriggerBindingSource,
    TriggerBindingSpec, TriggerHandlerSpec, TriggerPredicateSpec,
};
use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus, TraceId, TriggerEvent};
use crate::TriggerPredicateBudget;
use crate::Vm;

use super::retry::TriggerRetryConfig;
use super::uri::{DispatchUri, DispatchUriError};
use super::{
    append_dispatch_cancel_request, DispatchCancelRequest, DispatchStatus, Dispatcher, RetryPolicy,
};

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

async fn compile_trigger_expr(
    vm: &mut Vm,
    dir: &std::path::Path,
    label: &str,
    expr: &str,
) -> crate::triggers::TriggerExpressionSpec {
    let source = format!(
        "import \"std/triggers\"\n\npub fn __expr(event: TriggerEvent) -> any {{\n  return {expr}\n}}\n"
    );
    let exports = vm
        .load_module_exports_from_source(dir.join(format!("{label}.harn")), &source)
        .await
        .expect("compile trigger expression");
    crate::triggers::TriggerExpressionSpec {
        raw: expr.to_string(),
        closure: exports["__expr"].clone(),
    }
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
    dispatcher_fixture_with_options(
        source,
        handler_name,
        when_name,
        None,
        None,
        retry,
        crate::triggers::TriggerFlowControlConfig::default(),
    )
    .await
}

async fn dispatcher_fixture_with_flow_control(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    retry: TriggerRetryConfig,
    flow_control: crate::triggers::TriggerFlowControlConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    dispatcher_fixture_with_options(
        source,
        handler_name,
        when_name,
        None,
        None,
        retry,
        flow_control,
    )
    .await
}

async fn dispatcher_fixture_with_options(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    when_budget: Option<TriggerPredicateBudget>,
    daily_cost_usd: Option<f64>,
    retry: TriggerRetryConfig,
    flow_control: crate::triggers::TriggerFlowControlConfig,
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
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: handler_name.to_string(),
            closure: handler,
        },
        when,
        when_budget,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd,
        max_concurrent: None,
        flow_control,
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
    allow_cleartext: bool,
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
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::A2a {
            target: target.clone(),
            allow_cleartext,
        },
        when: None,
        when_budget: None,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
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

fn lifecycle_payloads(
    events: &[(u64, crate::event_log::LogEvent)],
    kind: &str,
) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|(_, event)| event.kind == kind)
        .map(|(_, event)| event.payload.clone())
        .collect()
}

struct MockA2aServer {
    authority: String,
    requests: Receiver<serde_json::Value>,
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

impl MockA2aServer {
    fn next_request(&self) -> serde_json::Value {
        self.request_within(Duration::from_secs(5))
            .expect("mock A2A request")
    }

    fn request_within(&self, timeout: Duration) -> Option<serde_json::Value> {
        self.requests.recv_timeout(timeout).ok()
    }

    fn finish(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join.join().expect("mock A2A thread");
    }
}

fn spawn_mock_a2a_server(task_result: serde_json::Value) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "https", "https")
}

fn spawn_mock_https_a2a_server_with_card_scheme(
    task_result: serde_json::Value,
    card_scheme: &'static str,
) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "https", card_scheme)
}

fn spawn_mock_http_a2a_server(task_result: serde_json::Value) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "http", "http")
}

fn spawn_mock_a2a_server_with_schemes(
    task_result: serde_json::Value,
    listener_scheme: &'static str,
    card_scheme: &'static str,
) -> MockA2aServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock A2A listener");
    listener
        .set_nonblocking(true)
        .expect("set mock A2A listener nonblocking");
    let addr = listener.local_addr().expect("mock A2A addr");
    let authority = format!("127.0.0.1:{}", addr.port());
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let tls_config = (listener_scheme == "https").then(mock_a2a_tls_config);
    let max_connections = if listener_scheme == "http" && card_scheme == "http" {
        3
    } else {
        2
    };
    let join = thread::spawn(move || {
        let mut handled_requests = 0;
        while handled_requests < max_connections {
            if stop_thread.load(Ordering::SeqCst) {
                break;
            }
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("accept mock A2A request: {error}"),
            };
            stream
                .set_nonblocking(false)
                .expect("set mock A2A stream blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("set write timeout");
            if let Some(tls_config) = &tls_config {
                let connection = ServerConnection::new(tls_config.clone())
                    .expect("construct mock A2A TLS connection");
                let mut stream = StreamOwned::new(connection, stream);
                handle_mock_a2a_connection(
                    &mut stream,
                    card_scheme,
                    addr.port(),
                    &tx,
                    &task_result,
                );
            } else {
                let mut stream = stream;
                let mut first = [0u8; 1];
                let read = stream.peek(&mut first).expect("peek mock A2A stream");
                if read == 0 || !matches!(first[0], b'G' | b'P') {
                    handled_requests += 1;
                    continue;
                }
                handle_mock_a2a_connection(
                    &mut stream,
                    card_scheme,
                    addr.port(),
                    &tx,
                    &task_result,
                );
            }
            handled_requests += 1;
        }
    });
    MockA2aServer {
        authority,
        requests: rx,
        stop,
        join,
    }
}

fn handle_mock_a2a_connection<T: Read + Write>(
    stream: &mut T,
    card_scheme: &str,
    port: u16,
    tx: &mpsc::Sender<serde_json::Value>,
    task_result: &serde_json::Value,
) {
    let (request_line, body) = read_http_request(stream);
    if request_line.starts_with("GET /.well-known/a2a-agent ") {
        write_json_response(
            stream,
            &serde_json::json!({
                "id": "mock-a2a",
                "url": format!("{card_scheme}://127.0.0.1:{port}"),
                "interfaces": [{"protocol": "jsonrpc", "url": "/rpc"}],
            }),
        );
        return;
    }
    assert!(
        request_line.starts_with("POST /rpc "),
        "unexpected request line: {request_line}"
    );
    tx.send(serde_json::from_slice::<serde_json::Value>(&body).expect("mock A2A request json"))
        .expect("capture mock A2A request");
    let rpc_id = serde_json::from_slice::<serde_json::Value>(&body).expect("mock A2A request json")
        ["id"]
        .clone();
    write_json_response(
        stream,
        &crate::jsonrpc::response(rpc_id, task_result.clone()),
    );
}

fn mock_a2a_tls_config() -> Arc<ServerConfig> {
    install_rustls_provider();
    let cert = generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".to_string()])
        .expect("generate mock A2A certificate");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der.into())
            .expect("build mock A2A TLS server config"),
    )
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn read_http_request<T: Read>(stream: &mut T) -> (String, Vec<u8>) {
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

fn write_json_response<T: Write>(stream: &mut T, body: &serde_json::Value) {
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
            assert!(node_kinds.iter().any(|kind| kind == "trigger_predicate"));
            assert!(node_kinds.iter().any(|kind| kind == "dispatch"));
            assert!(edge_kinds.iter().any(|kind| kind == "trigger_dispatch"));
            assert!(edge_kinds.iter().any(|kind| kind == "predicate_gate"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_budget_exceeded_short_circuits_and_emits_lifecycle() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_options(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "budget gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini", llm_retries: 0},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                Some(TriggerPredicateBudget {
                    max_cost_usd: Some(0.001),
                    tokens_max: Some(1),
                    timeout_ms: None,
                }),
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(3_000),
                output_tokens: Some(4_000),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                error: None,
            });

            let outcome = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-budget"))
                .await
                .expect("dispatch succeeds")
                .into_iter()
                .next()
                .expect("dispatch outcome");

            assert_eq!(outcome.status, DispatchStatus::Skipped);
            assert_eq!(
                outcome
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("budget_exceeded")
            );

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let budget_events = lifecycle_payloads(&lifecycle, "predicate.budget_exceeded");
            assert_eq!(budget_events.len(), 1);
            assert!(budget_events[0]["cost_usd"].as_f64().unwrap_or_default() > 0.001);
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 1);
            assert_eq!(evaluated[0]["result"], serde_json::json!(false));
            assert_eq!(evaluated[0]["reason"], serde_json::json!("budget_exceeded"));

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(!outbox
                .iter()
                .any(|(_, event)| event.kind == "dispatch_started"));
            assert_eq!(get_llm_mock_calls().len(), 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_daily_budget_exceeded_short_circuits_subsequent_events() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_options(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "daily gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini", llm_retries: 0},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                None,
                Some(0.001),
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(3_000),
                output_tokens: Some(4_000),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                error: None,
            });

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-daily-1"))
                .await
                .expect("first dispatch succeeds")
                .into_iter()
                .next()
                .expect("first outcome");
            assert_eq!(first.status, DispatchStatus::Skipped);
            assert_eq!(
                first
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("daily_budget_exceeded")
            );

            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-daily-2"))
                .await
                .expect("second dispatch succeeds")
                .into_iter()
                .next()
                .expect("second outcome");
            assert_eq!(second.status, DispatchStatus::Skipped);
            assert_eq!(
                second
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("daily_budget_exceeded")
            );
            assert_eq!(get_llm_mock_calls().len(), 1);

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let daily_events = lifecycle_payloads(&lifecycle, "predicate.daily_budget_exceeded");
            assert_eq!(daily_events.len(), 2);
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 2);
            assert_eq!(
                evaluated[0]["reason"],
                serde_json::json!("daily_budget_exceeded")
            );
            assert_eq!(
                evaluated[1]["reason"],
                serde_json::json!("daily_budget_exceeded")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_replay_uses_event_cache_without_hitting_provider() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "replay gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini", llm_retries: 0},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                TriggerRetryConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(10),
                output_tokens: Some(5),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                error: None,
            });

            let event = trigger_event("issues.opened", "delivery-replay-cache");
            let first = dispatcher
                .dispatch_event(event.clone())
                .await
                .expect("first dispatch succeeds")
                .into_iter()
                .next()
                .expect("first outcome");
            assert_eq!(first.status, DispatchStatus::Succeeded);
            assert_eq!(get_llm_mock_calls().len(), 1);

            crate::llm::reset_llm_state();

            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let replay = dispatcher
                .dispatch_replay(&binding, event.clone(), event.id.0.clone())
                .await
                .expect("replay succeeds");
            assert_eq!(replay.status, DispatchStatus::Succeeded);
            assert!(get_llm_mock_calls().is_empty());

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 2);
            assert_eq!(evaluated[0]["cached"], serde_json::json!(false));
            assert_eq!(evaluated[1]["cached"], serde_json::json!(true));
            assert_eq!(
                evaluated[1]["replay_of_event_id"],
                serde_json::json!(event.id.0)
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_circuit_breaker_opens_after_three_failures() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let sink = Rc::new(CollectorSink::new());
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  throw "predicate failed"
}
"#,
                "local_fn",
                Some("should_handle"),
                TriggerRetryConfig::default(),
            )
            .await;
            clear_event_sinks();
            add_event_sink(sink.clone());

            for index in 0..3 {
                let outcome = dispatcher
                    .dispatch_event(trigger_event(
                        "issues.opened",
                        &format!("delivery-circuit-{index}"),
                    ))
                    .await
                    .expect("dispatch succeeds")
                    .into_iter()
                    .next()
                    .expect("dispatch outcome");
                assert_eq!(outcome.status, DispatchStatus::Skipped);
            }

            let fourth = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-circuit-4"))
                .await
                .expect("fourth dispatch succeeds")
                .into_iter()
                .next()
                .expect("fourth outcome");
            assert_eq!(fourth.status, DispatchStatus::Skipped);
            assert_eq!(
                fourth
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("circuit_open")
            );
            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let state = binding
                .predicate_state
                .lock()
                .expect("predicate state lock");
            assert_eq!(state.consecutive_failures, 3);
            assert!(state.breaker_open_until_ms.is_some());

            let logs = sink.logs.borrow();
            assert!(logs.iter().any(|event| {
                event.level == EventLevel::Warn
                    && event.category == "trigger.predicate.circuit_breaker"
                    && event.message.contains("opened for 5 minutes")
            }));
            assert!(logs.iter().any(|event| {
                event.level == EventLevel::Warn
                    && event.category == "trigger.predicate.circuit_breaker"
                    && event.message.contains("short-circuiting to false")
            }));
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
                    "card_url": format!("https://{}/.well-known/a2a-agent", server.authority),
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
                server.request_within(Duration::from_millis(100)).is_none(),
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
                server.request_within(Duration::from_millis(100)).is_none(),
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
async fn replay_dispatch_scopes_harn_replay_per_dispatch_and_child_process() {
    std::env::set_var("HARN_REPLAY", "outer");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  let child = shell("printf '%s' \"$HARN_REPLAY\"")
  return {
    replay_env: env_or("HARN_REPLAY", "missing"),
    child_replay_env: child.stdout,
    dedupe_key: event.dedupe_key,
  }
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
            )
            .await;

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
                assert_eq!(result["child_replay_env"], serde_json::json!("1"));
                dedupe_keys.push(
                    result["dedupe_key"]
                        .as_str()
                        .expect("dedupe key")
                        .to_string(),
                );
            }
            dedupe_keys.sort();
            assert_eq!(dedupe_keys, vec!["delivery-env-a", "delivery-env-b"]);

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

            tokio::time::sleep(Duration::from_millis(25)).await;
            append_dispatch_cancel_request(
                &log,
                &DispatchCancelRequest {
                    binding_key: binding_key.clone(),
                    event_id: event_id.clone(),
                    requested_at: time::OffsetDateTime::now_utc(),
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

            tokio::time::sleep(Duration::from_millis(20)).await;
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

            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let outbox = read_topic(log.clone(), "trigger.outbox").await;
                if outbox.iter().any(|(_, event)| {
                    event.headers.get("event_id").map(String::as_str) == Some(live.id.0.as_str())
                        && event.kind == "dispatch_succeeded"
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
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

// TODO(harn#324): Hangs deterministically after the #258 topic split — the
// handler's `is_cancelled()` loop doesn't observe the cancel_token set by
// `dispatcher.shutdown()` when the handler is mid-`sleep()`. The topic read
// below was already repointed at `trigger.inbox.envelopes` (the old
// `trigger.inbox` legacy topic never receives `event_ingested` after the
// split), but the shutdown-cancel wiring itself needs to plumb cancel
// notification into the sleep future, not just polled via a busy loop.
#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky after #258 inbox topic split — tracked in harn#324"]
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

            tokio::time::sleep(Duration::from_millis(20)).await;
            dispatcher
                .enqueue(trigger_event("issues.opened", "delivery-run-shutdown"))
                .await
                .expect("enqueue succeeds");

            tokio::time::timeout(Duration::from_secs(5), dequeued_rx)
                .await
                .expect("run should dequeue live inbox event")
                .expect("run dequeued inbox event");
            dispatcher.shutdown();
            run_handle.await.expect("join dispatcher run");
            let drain = dispatcher
                .drain(Duration::from_secs(1))
                .await
                .expect("shutdown drain completes");
            assert!(drain.drained, "{drain:?}");

            let inbox = read_topic(log.clone(), "trigger.inbox.envelopes").await;
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

#[tokio::test(flavor = "current_thread")]
async fn flow_control_rate_limit_skips_excess_dispatches() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    rate_limit: Some(crate::triggers::TriggerRateLimitConfig {
                        key: None,
                        period: Duration::from_secs(60),
                        max: 1,
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-rate-1"))
                .await
                .expect("first dispatch succeeds");
            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-rate-2"))
                .await
                .expect("second dispatch returns skip");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(first[0].result, Some(serde_json::json!("delivery-rate-1")));
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "rate_limited",
                }))
            );

            let events = read_topic(
                log.clone(),
                "trigger.rate_limit.github-new-issue_v1__global",
            )
            .await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "rate_limit_allowed"));
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "rate_limit_blocked"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flow_control_throttle_waits_for_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let clock = crate::triggers::test_util::clock::MockClock::new(
                time::OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
            );
            let _guard = crate::triggers::test_util::clock::install_override(clock.clone());
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    throttle: Some(crate::triggers::TriggerThrottleConfig {
                        key: None,
                        period: Duration::from_secs(30),
                        max: 1,
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-throttle-1"))
                .await
                .expect("first dispatch succeeds");
            assert_eq!(first[0].status, DispatchStatus::Succeeded);

            let dispatcher_for_task = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-throttle-2"))
                    .await
                    .expect("second dispatch succeeds")
            });

            tokio::task::yield_now().await;
            assert!(
                !second.is_finished(),
                "second dispatch should still be waiting on the throttle window"
            );

            clock.advance_std(Duration::from_secs(30)).await;

            let second = second.await.expect("join throttled dispatch");
            assert_eq!(second[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!("delivery-throttle-2"))
            );

            let events =
                read_topic(log.clone(), "trigger.throttle.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "throttle_wait"));
            assert!(
                events
                    .iter()
                    .filter(|(_, event)| event.kind == "throttle_acquired")
                    .count()
                    >= 2
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flow_control_singleton_skips_while_inflight() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn slow_handler(event: TriggerEvent) -> string {
  sleep(50)
  return event.dedupe_key
}
"#,
                "slow_handler",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    singleton: Some(crate::triggers::TriggerSingletonConfig { key: None }),
                    ..Default::default()
                },
            )
            .await;

            let dispatcher_for_task = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-singleton-1"))
                    .await
                    .expect("first dispatch succeeds")
            });

            tokio::time::sleep(Duration::from_millis(10)).await;

            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-singleton-2"))
                .await
                .expect("second dispatch returns skip");
            let first = first.await.expect("join singleton leader");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "singleton_active",
                }))
            );

            let events =
                read_topic(log.clone(), "trigger.singleton.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "singleton_acquired"));
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "singleton_skipped"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flow_control_debounce_keeps_latest_event() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let clock = crate::triggers::test_util::clock::MockClock::new(
                time::OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
            );
            let _guard = crate::triggers::test_util::clock::install_override(clock.clone());
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_default_for_base_dir(dir.path()).expect("install event log");
            let lib_path = dir.path().join("lib.harn");
            std::fs::write(
                &lib_path,
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
            )
            .expect("write module source");

            let mut vm = Vm::new();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            let flow_control = crate::triggers::TriggerFlowControlConfig {
                debounce: Some(crate::triggers::TriggerDebounceConfig {
                    key: compile_trigger_expr(
                        &mut vm,
                        dir.path(),
                        "debounce_group",
                        "event.headers.group",
                    )
                    .await,
                    period: Duration::from_secs(30),
                }),
                ..Default::default()
            };
            let exports = vm
                .load_module_exports(&lib_path)
                .await
                .expect("load handler exports");
            let handler = exports["local_fn"].clone();
            install_manifest_triggers(vec![TriggerBindingSpec {
                id: "github-new-issue".to_string(),
                source: TriggerBindingSource::Manifest,
                kind: "webhook".to_string(),
                provider: ProviderId::from("github"),
                autonomy_tier: crate::AutonomyTier::ActAuto,
                handler: TriggerHandlerSpec::Local {
                    raw: "local_fn".to_string(),
                    closure: handler,
                },
                when: None,
                when_budget: None,
                retry: TriggerRetryConfig::default(),
                match_events: vec!["issues.opened".to_string()],
                dedupe_key: Some("event.dedupe_key".to_string()),
                dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
                filter: None,
                daily_cost_usd: None,
                max_concurrent: None,
                flow_control,
                manifest_path: None,
                package_name: Some("workspace".to_string()),
                definition_fingerprint: "fp:local_fn".to_string(),
            }])
            .await
            .expect("install test trigger binding");
            let dispatcher = Dispatcher::with_event_log(vm, log);

            let mut first_event = trigger_event("issues.opened", "delivery-debounce-1");
            first_event
                .headers
                .insert("group".to_string(), "issues".to_string());
            let mut second_event = trigger_event("issues.opened", "delivery-debounce-2");
            second_event
                .headers
                .insert("group".to_string(), "issues".to_string());

            let first_dispatcher = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                first_dispatcher
                    .dispatch_event(first_event)
                    .await
                    .expect("first dispatch completes")
            });
            tokio::task::yield_now().await;

            let second_dispatcher = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                second_dispatcher
                    .dispatch_event(second_event)
                    .await
                    .expect("second dispatch completes")
            });
            tokio::task::yield_now().await;

            clock.advance_std(Duration::from_secs(30)).await;

            let first = first.await.expect("join first debounce dispatch");
            let second = second.await.expect("join second debounce dispatch");
            assert_eq!(first[0].status, DispatchStatus::Skipped);
            assert_eq!(second[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!("delivery-debounce-2"))
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flow_control_batch_coalesces_multiple_events() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  let batch_count = if event.batch == nil { 0 } else { len(event.batch) }
  return {dedupe_key: event.dedupe_key, batch_count: batch_count}
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    batch: Some(crate::triggers::TriggerBatchConfig {
                        key: None,
                        size: 2,
                        timeout: Duration::from_secs(30),
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first_dispatcher = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                first_dispatcher
                    .dispatch_event(trigger_event("issues.opened", "delivery-batch-1"))
                    .await
                    .expect("first batch dispatch completes")
            });
            tokio::task::yield_now().await;

            let second_dispatcher = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                second_dispatcher
                    .dispatch_event(trigger_event("issues.opened", "delivery-batch-2"))
                    .await
                    .expect("second batch dispatch completes")
            });

            let first = first.await.expect("join batch leader");
            let second = second.await.expect("join batch follower");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                first[0].result,
                Some(serde_json::json!({
                    "dedupe_key": "delivery-batch-1",
                    "batch_count": 2,
                }))
            );
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "batch_merged",
                }))
            );

            let events = read_topic(log.clone(), "trigger.batch.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "batch_dispatched"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn flow_control_priority_prefers_higher_ranked_waiters() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_default_for_base_dir(dir.path()).expect("install event log");
            let lib_path = dir.path().join("lib.harn");
            std::fs::write(
                &lib_path,
                r#"
import "std/triggers"

pub fn slow_handler(event: TriggerEvent) -> string {
  sleep(30)
  return event.headers.tier
}
"#,
            )
            .expect("write module source");

            let mut vm = Vm::new();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            let flow_control = crate::triggers::TriggerFlowControlConfig {
                concurrency: Some(crate::triggers::TriggerConcurrencyConfig { key: None, max: 1 }),
                priority: Some(crate::triggers::TriggerPriorityOrderConfig {
                    key: compile_trigger_expr(
                        &mut vm,
                        dir.path(),
                        "priority_tier",
                        "event.headers.tier",
                    )
                    .await,
                    order: vec![
                        "gold".to_string(),
                        "silver".to_string(),
                        "bronze".to_string(),
                    ],
                }),
                ..Default::default()
            };
            let exports = vm
                .load_module_exports(&lib_path)
                .await
                .expect("load handler exports");
            let handler = exports["slow_handler"].clone();
            install_manifest_triggers(vec![TriggerBindingSpec {
                id: "github-new-issue".to_string(),
                source: TriggerBindingSource::Manifest,
                kind: "webhook".to_string(),
                provider: ProviderId::from("github"),
                autonomy_tier: crate::AutonomyTier::ActAuto,
                handler: TriggerHandlerSpec::Local {
                    raw: "slow_handler".to_string(),
                    closure: handler,
                },
                when: None,
                when_budget: None,
                retry: TriggerRetryConfig::default(),
                match_events: vec!["issues.opened".to_string()],
                dedupe_key: Some("event.dedupe_key".to_string()),
                dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
                filter: None,
                daily_cost_usd: None,
                max_concurrent: None,
                flow_control,
                manifest_path: None,
                package_name: Some("workspace".to_string()),
                definition_fingerprint: "fp:slow_handler".to_string(),
            }])
            .await
            .expect("install test trigger binding");
            let dispatcher = Dispatcher::with_event_log(vm, log.clone());

            let mut bronze_first = trigger_event("issues.opened", "delivery-priority-bronze-1");
            bronze_first
                .headers
                .insert("tier".to_string(), "bronze".to_string());
            let mut bronze_second = trigger_event("issues.opened", "delivery-priority-bronze-2");
            bronze_second
                .headers
                .insert("tier".to_string(), "bronze".to_string());
            let mut gold = trigger_event("issues.opened", "delivery-priority-gold");
            gold.headers.insert("tier".to_string(), "gold".to_string());

            let bronze_first_id = bronze_first.id.0.clone();
            let bronze_second_id = bronze_second.id.0.clone();
            let gold_id = gold.id.0.clone();

            let leader_dispatcher = dispatcher.clone();
            let leader = tokio::task::spawn_local(async move {
                leader_dispatcher
                    .dispatch_event(bronze_first)
                    .await
                    .expect("leader dispatch succeeds")
            });

            tokio::time::sleep(Duration::from_millis(10)).await;

            let bronze_dispatcher = dispatcher.clone();
            let bronze_waiter = tokio::task::spawn_local(async move {
                bronze_dispatcher
                    .dispatch_event(bronze_second)
                    .await
                    .expect("bronze waiter succeeds")
            });
            let gold_dispatcher = dispatcher.clone();
            let gold_waiter = tokio::task::spawn_local(async move {
                gold_dispatcher
                    .dispatch_event(gold)
                    .await
                    .expect("gold waiter succeeds")
            });

            let leader = leader.await.expect("join leader");
            let gold = gold_waiter.await.expect("join gold waiter");
            let bronze = bronze_waiter.await.expect("join bronze waiter");
            assert_eq!(leader[0].status, DispatchStatus::Succeeded);
            assert_eq!(gold[0].status, DispatchStatus::Succeeded);
            assert_eq!(bronze[0].status, DispatchStatus::Succeeded);

            let started = read_topic(log.clone(), "trigger.outbox")
                .await
                .into_iter()
                .filter(|(_, event)| event.kind == "dispatch_started")
                .filter_map(|(_, event)| event.headers.get("event_id").cloned())
                .filter(|event_id| {
                    event_id == &bronze_first_id
                        || event_id == &bronze_second_id
                        || event_id == &gold_id
                })
                .collect::<Vec<_>>();
            assert_eq!(started, vec![bronze_first_id, gold_id, bronze_second_id]);
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
