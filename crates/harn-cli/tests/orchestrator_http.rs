#![cfg(unix)]
// Orchestrator HTTP tests serialize CLI child processes with the shared
// cross-process file lock. Each test holds the guard across .await boundaries
// while the child server is alive.
#![allow(clippy::await_holding_lock)]

mod support;
mod test_util;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use harn_vm::event_log::{EventLog, SqliteEventLog, Topic};
use hmac::{Hmac, KeyInit, Mac};
use rcgen::generate_simple_self_signed;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
use reqwest::Certificate;
use reqwest::StatusCode;
use serde_json::Value as JsonValue;
use sha2::Sha256;
use tempfile::TempDir;
use test_util::process::harn_command;
use test_util::timing::{
    self, ChildExitWatcher, EVENT_FAIL_FAST_TIMEOUT, LOG_RECV_POLL_INTERVAL,
    PROCESS_FAIL_FAST_TIMEOUT,
};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const STARTUP_PREFIX: &str = "[harn] HTTP listener ready on ";
const STARTUP_NEEDLE: &str = "HTTP listener ready";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";
type HmacSha256 = Hmac<Sha256>;

fn lock_orchestrator_tests() -> support::OrchestratorProcessTestLock {
    support::lock_orchestrator_process_tests()
}

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_bytes(dir: &Path, relative: &str, bytes: &[u8]) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn base_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn github_harn_override_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[providers]]
id = "github"
connector = { harn = "github_connector.harn" }

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#
}

fn github_marker_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {{
  write_file({marker:?}, event.kind)
}}
"#,
        marker = marker_path.display().to_string()
    )
}

fn slack_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "slack-mentions"
kind = "webhook"
provider = "slack"
match = { events = ["app_mention"] }
handler = "handlers::on_slack"
secrets = { signing_secret = "slack/signing-secret" }
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn notion_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "notion-pages"
kind = "webhook"
provider = "notion"
path = "/hooks/notion"
match = { path = "/hooks/notion", events = ["page.content_updated"] }
handler = "handlers::on_notion"
secrets = { verification_token = "notion/verification-token" }
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn echo_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[providers]]
id = "echo"
connector = { harn = "echo_connector.harn" }

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "handlers::on_echo"
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn stream_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "ws-stream"
kind = "stream"
provider = "websocket"
path = "/streams/ws"
match = { events = ["quote.tick"] }
handler = "handlers::on_stream"
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn slack_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_slack(event: TriggerEvent) {{
  write_file({marker:?}, event.kind)
}}
"#,
        marker = marker_path.display().to_string()
    )
}

fn notion_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_notion(event: TriggerEvent) {{
  write_file({marker:?}, event.kind)
}}
"#,
        marker = marker_path.display().to_string()
    )
}

fn echo_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_echo(event: TriggerEvent) {{
  let ping = connector_call("echo", "ping", {{
    message: event.provider_payload.raw.body.message,
  }})
  write_file({marker:?}, json_stringify({{
    kind: event.kind,
    token: event.provider_payload.raw.token,
    binding_id: event.provider_payload.raw.binding_id,
    echoed: ping.message,
    ping_token: ping.token,
  }}))
}}
"#,
        marker = marker_path.display().to_string()
    )
}

fn stream_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_stream(event: TriggerEvent) {{
  write_file({marker:?}, json_stringify({{
    provider: event.provider,
    kind: event.kind,
    key: event.provider_payload.key,
    stream: event.provider_payload.stream,
    amount: event.provider_payload.raw.value.amount,
  }}))
}}
"#,
        marker = marker_path.display().to_string()
    )
}

fn echo_connector_module() -> &'static str {
    r#"
var active_bindings = []

pub fn provider_id() {
  return "echo"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return {
    harn_schema_name: "EchoEventPayload",
    json_schema: {
      type: "object",
      additionalProperties: true,
    },
  }
}

pub fn init(_ctx) {
  event_log_emit("connectors.echo.lifecycle", "init", {phase: "init"})
}

pub fn activate(bindings) {
  active_bindings = bindings
  metrics_inc("echo_activate_bindings", len(bindings))
  event_log_emit("connectors.echo.lifecycle", "activate", {
    binding_count: len(bindings),
  })
}

pub fn shutdown() {
  event_log_emit("connectors.echo.lifecycle", "shutdown", {
    binding_count: len(active_bindings),
  })
}

pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  let token = secret_get("echo/api-token")
  metrics_inc("echo_normalize_calls")
  event_log_emit("connectors.echo.lifecycle", "normalize", {
    binding_id: raw.binding_id,
    message: body.message,
  })
  return {
    type: "event",
    event: {
      kind: "echo.received",
      occurred_at: raw.received_at,
      dedupe_key: "echo:" + body.id,
      payload: {
        body: body,
        token: token,
        binding_id: raw.binding_id,
      },
    },
  }
}

pub fn call(method, args) {
  if method == "ping" {
    metrics_inc("echo_client_calls")
    event_log_emit("connectors.echo.calls", "ping", {
      message: args.message,
    })
    return {
      message: args.message,
      token: secret_get("echo/api-token"),
    }
  }

  throw "method_not_found:" + method
}
"#
}

fn github_override_connector_module() -> &'static str {
    r#"
pub fn provider_id() {
  return "github"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return "GitHubEventPayload"
}

pub fn init(_ctx) {
  event_log_emit("connectors.github.override", "init", {provider: "github"})
}

pub fn activate(bindings) {
  metrics_inc("github_override_activate_bindings", len(bindings))
}

pub fn normalize_inbound(raw) {
  let body = raw.body_json ?? json_parse(raw.body_text)
  event_log_emit("connectors.github.override", "normalize", {
    id: body.id,
    action: body.action,
  })
  return {
    type: "event",
    event: {
      kind: raw.headers["X-GitHub-Event"] ?? raw.headers["x-github-event"],
      dedupe_key: "harn-github:" + body.id,
      payload: body,
      signature_status: {state: "unsigned"},
    },
  }
}

pub fn call(method, _args) {
  throw "method_not_found:" + method
}
"#
}

fn a2a_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
path = "/a2a/review"
match = { events = ["a2a.task.received"] }
handler = "handlers::on_task"
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn a2a_handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) {
  log(event.kind)
}
"#
}

fn spawn_orchestrator(
    temp: &TempDir,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> OrchestratorProcess {
    let mut command = harn_command();
    command
        .current_dir(temp.path())
        .arg("orchestrator")
        .arg("serve")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .arg("--role")
        .arg("single-tenant")
        .arg("--bind")
        .arg("127.0.0.1:0")
        // The 30s default is calibrated for production drains; tests don't
        // queue real backlogs, so cap shutdown at 5s to keep flake-recovery
        // bounded.
        .arg("--shutdown-timeout")
        .arg("5")
        .stderr(Stdio::piped())
        .stdout(Stdio::null());
    for arg in extra_args {
        command.arg(arg);
    }
    for (key, value) in envs {
        command.env(key, value);
    }

    let mut child = command.spawn().unwrap();
    let stderr = child.stderr.take().expect("stderr pipe");
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut collected = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("stderr line");
            collected.push_str(&line);
            collected.push('\n');
            let _ = tx.send(line);
        }
        collected
    });

    OrchestratorProcess {
        child: ChildExitWatcher::new(child),
        rx,
        handle: Some(handle),
    }
}

struct OrchestratorProcess {
    child: ChildExitWatcher,
    rx: Receiver<String>,
    handle: Option<thread::JoinHandle<String>>,
}

impl OrchestratorProcess {
    fn wait_for_listener_url(&mut self) -> String {
        let deadline = Instant::now() + PROCESS_FAIL_FAST_TIMEOUT;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(LOG_RECV_POLL_INTERVAL) {
                Ok(line) if line.contains(STARTUP_NEEDLE) => {
                    if let Some(url) = listener_url_from_line(&line) {
                        support::wait_for_readyz(&mut self.child, &url, PROCESS_FAIL_FAST_TIMEOUT)
                            .unwrap_or_else(|error| {
                                let stderr = self.shutdown_and_join_stderr();
                                panic!("{error}\nstderr={stderr}");
                            });
                        return url;
                    }
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_status().unwrap() {
                        let stderr = self.join_stderr();
                        panic!(
                            "process exited before listener became ready: {status}\nstderr={stderr}"
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let stderr = self.shutdown_and_join_stderr();
                    panic!("stderr stream closed before listener became ready\nstderr={stderr}");
                }
            }
        }
        let stderr = self.shutdown_and_join_stderr();
        panic!("timed out waiting for listener startup\nstderr={stderr}");
    }

    fn shutdown_and_join_stderr(&mut self) -> String {
        self.child.kill();
        let _ = self.child.wait_timeout(PROCESS_FAIL_FAST_TIMEOUT);
        self.join_stderr()
    }

    fn join_stderr(&mut self) -> String {
        self.handle
            .take()
            .expect("stderr collector thread")
            .join()
            .expect("stderr collector result")
    }
}

fn listener_url_from_line(line: &str) -> Option<String> {
    if let Some(url) = line.split(STARTUP_PREFIX).nth(1) {
        return url
            .split_whitespace()
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
    }
    let field = "listener_url=";
    let start = line.find(field)? + field.len();
    let url = line[start..]
        .split_whitespace()
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(url.to_string())
}

fn send_sigterm(child: &mut ChildExitWatcher) {
    child.terminate();
}

fn wait_for_exit(child: &mut ChildExitWatcher) {
    child.wait_for_success(PROCESS_FAIL_FAST_TIMEOUT);
}

async fn wait_for_exit_async(child: &mut ChildExitWatcher) -> std::process::ExitStatus {
    child
        .wait_timeout(PROCESS_FAIL_FAST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"))
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256={encoded}")
}

fn slack_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    let mut encoded = String::new();
    for byte in mac.finalize().into_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("v0={encoded}")
}

fn github_headers(secret: &str, body: &[u8], origin: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("X-GitHub-Event", HeaderValue::from_static("issues"));
    headers.insert(
        "X-GitHub-Delivery",
        HeaderValue::from_static("delivery-123"),
    );
    headers.insert(
        "X-Hub-Signature-256",
        HeaderValue::from_str(&github_signature(secret, body)).unwrap(),
    );
    if let Some(origin) = origin {
        headers.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
    }
    headers
}

fn slack_headers(secret: &str, timestamp: i64, body: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "X-Slack-Request-Timestamp",
        HeaderValue::from_str(&timestamp.to_string()).unwrap(),
    );
    headers.insert(
        "X-Slack-Signature",
        HeaderValue::from_str(&slack_signature(secret, timestamp, body)).unwrap(),
    );
    headers
}

fn notion_headers(secret: &str, body: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "X-Notion-Signature",
        HeaderValue::from_str(&github_signature(secret, body)).unwrap(),
    );
    headers.insert("request-id", HeaderValue::from_static("req-notion-123"));
    headers
}

fn state_snapshot(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap()
}

async fn read_topic_events(
    temp: &TempDir,
    topic: &str,
) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
    let log = SqliteEventLog::open(temp.path().join("state/events.sqlite"), 32).unwrap();
    let topic = Topic::new(topic).unwrap();
    log.read_range(&topic, None, usize::MAX).await.unwrap()
}

async fn wait_for_topic_event(
    temp: &TempDir,
    topic: &str,
    predicate: impl Fn(&harn_vm::event_log::LogEvent) -> bool,
) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        if read_topic_events(temp, topic)
            .await
            .iter()
            .any(|(_, event)| predicate(event))
        {
            return;
        }
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    }
    let events = read_topic_events(temp, topic).await;
    panic!("timed out waiting for matching {topic} event; events={events:?}");
}

async fn assert_status(response: reqwest::Response, expected: StatusCode) {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, expected, "status={status} body={body}");
}

fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

fn wait_for_path(path: &Path, timeout: std::time::Duration) {
    timing::wait_for_nonempty_file(path, timeout);
}

fn wait_for_json_file(path: &Path, timeout: std::time::Duration) -> JsonValue {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            if !contents.is_empty() {
                match serde_json::from_str(&contents) {
                    Ok(value) => return value,
                    Err(_) if Instant::now() < deadline => {}
                    Err(error) => {
                        panic!(
                            "timed out waiting for valid JSON in {}: {error}; contents={contents:?}",
                            path.display()
                        );
                    }
                }
            }
        }
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(remaining) => remaining,
            None => break,
        };
        timing::sleep_blocking(remaining.min(timing::RETRY_POLL_INTERVAL));
    }
    panic!("timed out waiting for valid JSON in {}", path.display());
}

#[derive(Clone, Debug)]
struct OtlpRequest {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct MockOtelCollectorState {
    requests: Arc<Mutex<Vec<OtlpRequest>>>,
}

struct MockOtelCollector {
    url: String,
    requests: Arc<Mutex<Vec<OtlpRequest>>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockOtelCollector {
    fn start() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockOtelCollectorState {
            requests: requests.clone(),
        };
        let (url_tx, url_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let app = Router::new()
                    .route("/v1/traces", post(record_otlp_traces))
                    .with_state(state);
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                url_tx.send(format!("http://{addr}")).unwrap();
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });
        });

        Self {
            url: url_rx.recv().unwrap(),
            requests,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }

    fn collected_spans(&self) -> Vec<CollectedSpan> {
        let requests = self.requests.lock().unwrap().clone();
        requests
            .into_iter()
            .flat_map(|request| {
                serde_json::from_slice::<JsonValue>(&request.body)
                    .map(|body| collect_spans_from_body(&body))
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl Drop for MockOtelCollector {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct CollectedSpan {
    name: String,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    attributes: Vec<(String, JsonValue)>,
}

async fn record_otlp_traces(
    State(state): State<MockOtelCollectorState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let captured_headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect::<Vec<_>>();
    state.requests.lock().unwrap().push(OtlpRequest {
        headers: captured_headers,
        body: body.to_vec(),
    });
    StatusCode::OK
}

fn collect_spans_from_body(body: &JsonValue) -> Vec<CollectedSpan> {
    let mut spans = Vec::new();
    let Some(resource_spans) = body.get("resourceSpans").and_then(JsonValue::as_array) else {
        return spans;
    };

    for resource_span in resource_spans {
        let Some(scope_spans) = resource_span
            .get("scopeSpans")
            .and_then(JsonValue::as_array)
        else {
            continue;
        };
        for scope_span in scope_spans {
            let Some(otel_spans) = scope_span.get("spans").and_then(JsonValue::as_array) else {
                continue;
            };
            for span in otel_spans {
                spans.push(CollectedSpan {
                    name: span
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    trace_id: span
                        .get("traceId")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    span_id: span
                        .get("spanId")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    parent_span_id: span
                        .get("parentSpanId")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                        .filter(|value| !value.is_empty()),
                    attributes: span
                        .get("attributes")
                        .and_then(JsonValue::as_array)
                        .map(|attributes| {
                            attributes
                                .iter()
                                .filter_map(|attribute| {
                                    Some((
                                        attribute.get("key")?.as_str()?.to_string(),
                                        attribute.get("value")?.clone(),
                                    ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                });
            }
        }
    }

    spans
}

fn attribute_string(span: &CollectedSpan, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| {
            value
                .get("stringValue")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    value
                        .get("intValue")
                        .map(|value| {
                            value
                                .as_str()
                                .map(ToString::to_string)
                                .or_else(|| value.as_i64().map(|value| value.to_string()))
                        })
                        .and_then(|value| value)
                })
        })
}

#[tokio::test(flavor = "multi_thread")]
async fn github_webhook_delivery_is_accepted_and_persisted() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "integration-test-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let health = reqwest::get(format!("{base_url}/health")).await.unwrap();
    assert_status(health, StatusCode::OK).await;

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"status\": \"stopped\""),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_provider_prefers_configured_harn_connector_over_deprecated_rust_default() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("github-override-handler.txt");
    write_file(
        temp.path(),
        "harn.toml",
        &github_harn_override_manifest(None),
    );
    write_file(
        temp.path(),
        "lib.harn",
        &github_marker_handler_module(&marker_path),
    );
    write_file(
        temp.path(),
        "github_connector.harn",
        github_override_connector_module(),
    );

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", "override-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt-gh-override-1",
        "action": "opened",
        "issue": {"number": 42, "title": "Harn connector override"}
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .header(CONTENT_TYPE, "application/json")
        .header("X-GitHub-Event", "issues")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_path(&marker_path, EVENT_FAIL_FAST_TIMEOUT);
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "issues");

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(
        !stderr.contains("deprecated Rust-side connector"),
        "Harn connector overrides must suppress Rust sunset warnings; stderr={stderr}"
    );

    let lifecycle = read_topic_events(&temp, "connectors.github.override").await;
    let lifecycle_kinds: Vec<_> = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect();
    assert_eq!(lifecycle_kinds, vec!["init", "normalize"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn slack_webhook_acknowledges_before_handler_finishes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("slack-handler.txt");
    let release_path = temp.path().join("release-slack-dispatch");
    let release_path_value = release_path.to_string_lossy().into_owned();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(temp.path(), "lib.harn", &slack_handler_module(&marker_path));

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            release_path_value.as_str(),
        ),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "ZZZZZZWSxiZZZ2yIvs3peJ",
        "team_id": "T123ABC456",
        "api_app_id": "A123ABC456",
        "event": {
            "type": "app_mention",
            "user": "U123ABC456",
            "text": "What is the hour of the pearl, <@U0LAN0Z89>?",
            "ts": "1515449522.000016",
            "channel": "C123ABC456",
            "event_ts": "1515449522000016"
        },
        "type": "event_callback",
        "event_id": "Ev123ABC456",
        "event_time": 1515449522000016i64
    }))
    .unwrap();

    let response = tokio::time::timeout(
        timing::SLACK_ACK_TIMEOUT,
        reqwest::Client::new()
            .post(format!("{base_url}/triggers/slack-mentions"))
            .headers(slack_headers(secret, timestamp, &body))
            .body(body)
            .send(),
    )
    .await
    .unwrap_or_else(|_| panic!("slack ack path exceeded {:?}", timing::SLACK_ACK_TIMEOUT))
    .unwrap();
    assert_status(response, StatusCode::OK).await;
    assert!(
        !marker_path.exists(),
        "dispatch should not have completed before the HTTP ack"
    );
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_acked" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    assert!(
        !marker_path.exists(),
        "dispatch should still be blocked on the explicit release gate"
    );
    fs::write(&release_path, b"release").unwrap();
    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["event_log_id"] == serde_json::json!(1)
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "app_mention");

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn slack_url_verification_returns_plaintext_challenge() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &slack_handler_module(&temp.path().join("unused-slack-marker.txt")),
    );

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "legacy-token",
        "challenge": "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P",
        "type": "url_verification"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(slack_headers(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = response.text().await.unwrap();
    assert_eq!(
        response_body,
        "3eZbrw1aBm2rZgRNFdxV2595E9CY3gmdALWMmHkvFXO7tYXAYM8P"
    );

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn slack_bad_requests_set_no_retry_header_and_export_delivery_metrics() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &slack_handler_module(&temp.path().join("unused-slack-marker.txt")),
    );

    let secret = "slack-signing-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_SLACK_SIGNING_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let body = serde_json::to_vec(&serde_json::json!({
        "token": "ZZZZZZWSxiZZZ2yIvs3peJ",
        "team_id": "T123ABC456",
        "api_app_id": "A123ABC456",
        "event": {
            "type": "app_mention",
            "user": "U123ABC456",
            "text": "hello",
            "ts": "1515449522.000016",
            "channel": "C123ABC456",
            "event_ts": "1515449522000016"
        },
        "type": "event_callback",
        "event_id": "Ev123ABC456",
        "event_time": 1515449522
    }))
    .unwrap();

    let mut bad_headers = slack_headers(secret, timestamp, &body);
    bad_headers.insert(
        "X-Slack-Signature",
        HeaderValue::from_static(
            "v0=0000000000000000000000000000000000000000000000000000000000000000",
        ),
    );
    let bad = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(bad_headers)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        bad.headers()
            .get("x-slack-no-retry")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    let ok = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(slack_headers(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let metrics = reqwest::Client::new()
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("slack_events_delivery_success_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("slack_events_delivery_failure_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("slack_events_auto_disable_min_success_ratio 0.05"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("harn_http_requests_total{endpoint=\"/triggers/slack-mentions\",method=\"POST\",status=\"200\"} 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains(
            "harn_trigger_received_total{provider=\"slack\",trigger_id=\"slack-mentions\"} 2"
        ),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains(
            "harn_event_log_append_duration_seconds_bucket{le=\"+Inf\",topic=\"orchestrator.triggers.pending\""
        ),
        "metrics={metrics}"
    );

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn notion_webhook_handshake_is_captured_and_reported_by_doctor() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &notion_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &notion_handler_module(&temp.path().join("unused-notion-marker.txt")),
    );

    let envs = [("HARN_SECRET_PROVIDERS", "env")];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "verification_token": "secret_notion_test_token"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/notion"))
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload: JsonValue = response.json().await.unwrap();
    assert_eq!(
        payload.get("status").and_then(JsonValue::as_str),
        Some("handshake_captured")
    );
    assert_eq!(
        payload
            .get("verification_token")
            .and_then(JsonValue::as_str),
        Some("secret_notion_test_token")
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");

    let doctor = harn_command()
        .current_dir(temp.path())
        .arg("doctor")
        .arg("--no-network")
        .env("HARN_SECRET_PROVIDERS", "env")
        .env("HARN_EVENT_LOG_SQLITE_PATH", "state/events.sqlite")
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "doctor failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        stdout.contains("WARN  notion:notion-pages"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("captured verification_token=secret_notion_test_token"),
        "stdout={stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notion_webhook_signed_delivery_is_dispatched() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("notion-handler.txt");
    write_file(temp.path(), "harn.toml", &notion_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &notion_handler_module(&marker_path),
    );

    let secret = "secret-notion-live-token";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_NOTION_VERIFICATION_TOKEN", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt_notion_1",
        "timestamp": "2026-04-19T12:34:56Z",
        "type": "page.content_updated",
        "workspace_id": "ws_123",
        "subscription_id": "sub_123",
        "integration_id": "int_123",
        "attempt_number": 1,
        "entity": {
            "id": "page_123",
            "type": "page"
        }
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/notion"))
        .headers(notion_headers(secret, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_path(&marker_path, EVENT_FAIL_FAST_TIMEOUT);
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "page.content_updated");

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn harn_connector_module_round_trips_inbound_and_client_calls() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("echo-handler.json");
    write_file(temp.path(), "harn.toml", &echo_manifest(None));
    write_file(temp.path(), "lib.harn", &echo_handler_module(&marker_path));
    write_file(temp.path(), "echo_connector.harn", echo_connector_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_ECHO_API_TOKEN", "echo-secret-token"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt_echo_1",
        "message": "hello from echo"
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/hooks/echo"))
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    let marker = wait_for_json_file(&marker_path, EVENT_FAIL_FAST_TIMEOUT);
    assert_eq!(
        marker.get("kind").and_then(JsonValue::as_str),
        Some("echo.received")
    );
    assert_eq!(
        marker.get("token").and_then(JsonValue::as_str),
        Some("echo-secret-token")
    );
    assert_eq!(
        marker.get("binding_id").and_then(JsonValue::as_str),
        Some("echo-webhook")
    );
    assert_eq!(
        marker.get("echoed").and_then(JsonValue::as_str),
        Some("hello from echo")
    );
    assert_eq!(
        marker.get("ping_token").and_then(JsonValue::as_str),
        Some("echo-secret-token")
    );

    let metrics = reqwest::Client::new()
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("connector_custom_echo_activate_bindings_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("connector_custom_echo_normalize_calls_total 1"),
        "metrics={metrics}"
    );
    assert!(
        metrics.contains("connector_custom_echo_client_calls_total 1"),
        "metrics={metrics}"
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let lifecycle = read_topic_events(&temp, "connectors.echo.lifecycle").await;
    let lifecycle_kinds: Vec<_> = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect();
    assert_eq!(
        lifecycle_kinds,
        vec!["init", "activate", "normalize", "shutdown"]
    );
    let normalize_event = lifecycle
        .iter()
        .find(|(_, event)| event.kind == "normalize")
        .expect("normalize event");
    assert_eq!(
        normalize_event
            .1
            .payload
            .get("binding_id")
            .and_then(JsonValue::as_str),
        Some("echo-webhook")
    );
    assert_eq!(
        normalize_event
            .1
            .payload
            .get("message")
            .and_then(JsonValue::as_str),
        Some("hello from echo")
    );

    let calls = read_topic_events(&temp, "connectors.echo.calls").await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.kind, "ping");
    assert_eq!(
        calls[0]
            .1
            .payload
            .get("message")
            .and_then(JsonValue::as_str),
        Some("hello from echo")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_trigger_route_uses_generic_stream_connector() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("stream-handler.json");
    write_file(temp.path(), "harn.toml", &stream_manifest(None));
    write_file(
        temp.path(),
        "lib.harn",
        &stream_handler_module(&marker_path),
    );

    let mut process = spawn_orchestrator(&temp, &[], &[("HARN_SECRET_PROVIDERS", "env")]);
    let base_url = process.wait_for_listener_url();

    let response = reqwest::Client::new()
        .post(format!("{base_url}/streams/ws"))
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({
            "key": "acct-1",
            "stream": "quotes",
            "value": {"amount": 10}
        }))
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;
    let marker: JsonValue =
        serde_json::from_str(&fs::read_to_string(&marker_path).unwrap()).unwrap();
    assert_eq!(
        marker.get("provider").and_then(JsonValue::as_str),
        Some("websocket")
    );
    assert_eq!(
        marker.get("kind").and_then(JsonValue::as_str),
        Some("quote.tick")
    );
    assert_eq!(
        marker.get("key").and_then(JsonValue::as_str),
        Some("acct-1")
    );
    assert_eq!(
        marker.get("stream").and_then(JsonValue::as_str),
        Some("quotes")
    );
    assert_eq!(marker.get("amount").and_then(JsonValue::as_i64), Some(10));

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(
        stderr.contains("activated connectors: websocket(1)"),
        "stderr={stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a2a_push_route_requires_bearer_or_valid_hmac() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key-1,test-key-2"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-123"}}"#;

    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(json_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let mut wrong_hmac_headers = json_headers();
    wrong_hmac_headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!(
            "HMAC-SHA256 timestamp={timestamp},signature=AAAAAAAAAAAAAAAAAAAAAA=="
        ))
        .unwrap(),
    );
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(wrong_hmac_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let mut bearer_headers = json_headers();
    bearer_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key-2"));
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"received\": 3"), "snapshot={snapshot}");
    assert!(snapshot.contains("\"failed\": 2"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn embedded_mcp_endpoint_serves_orchestrator_tools_on_listener() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "mcp-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--mcp"], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer mcp-key"));
    let initialize = client
        .post(format!("{base_url}/mcp"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "clientInfo": { "name": "orchestrator-test", "version": "0" },
                "capabilities": { "harn": { "apiKey": "mcp-key" } }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(initialize.status(), StatusCode::OK);
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("MCP session header")
        .to_string();
    let initialize_body: JsonValue = initialize.json().await.unwrap();
    assert_eq!(
        initialize_body["result"]["serverInfo"]["name"],
        serde_json::json!("harn-orchestrator")
    );

    auth_headers.insert(
        "mcp-session-id",
        HeaderValue::from_str(&session_id).unwrap(),
    );
    let tools = client
        .post(format!("{base_url}/mcp"))
        .headers(auth_headers)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(tools.status(), StatusCode::OK);
    let tools_body: JsonValue = tools.json().await.unwrap();
    assert!(
        tools_body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "harn.orchestrator.inspect"),
        "tools={tools_body}"
    );

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(
        stderr.contains("embedded MCP server mounted at /mcp"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_endpoint_applies_manifest_changes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    let original = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers.clone())
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-before"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(original, StatusCode::OK).await;

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-v2"),
    );

    let reload = client
        .post(format!("{base_url}/admin/reload"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({"source": "http_test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::OK);
    let reload_body: JsonValue = reload.json().await.unwrap();
    assert_eq!(reload_body["status"], serde_json::json!("ok"));
    assert_eq!(reload_body["source"], serde_json::json!("http_test"));
    assert_eq!(
        reload_body["summary"]["modified"][0],
        serde_json::json!("incoming-review-task")
    );

    let updated = client
        .post(format!("{base_url}/a2a/review-v2"))
        .headers(auth_headers.clone())
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-after"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(updated, StatusCode::OK).await;

    let retired = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-old"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(retired, StatusCode::NOT_FOUND).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"listener_url\""), "snapshot={snapshot}");
    assert!(snapshot.contains("\"version\": 2"), "snapshot={snapshot}");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_reload_invalid_manifest_keeps_existing_routes_live() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    write_file(
        temp.path(),
        "harn.toml",
        "[package]\nname = \"broken\"\n[[triggers]]\nid = ",
    );

    let reload = client
        .post(format!("{base_url}/admin/reload"))
        .headers(auth_headers.clone())
        .json(&serde_json::json!({"source": "http_test_invalid"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = reload.text().await.unwrap();
    assert!(body.contains("error"), "{body}");

    let still_live = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-still-live"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(still_live, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_mode_reloads_manifest_changes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--watch"], &envs);
    let base_url = process.wait_for_listener_url();

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-watch"),
    );

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));

    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    loop {
        let response = client
            .post(format!("{base_url}/a2a/review-watch"))
            .headers(auth_headers.clone())
            .body(br#"{"kind":"a2a.task.received","task":{"id":"task-watch"}}"#.to_vec())
            .send()
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            break;
        }
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(Instant::now() < deadline, "watch reload never applied");
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    }

    let retired = client
        .post(format!("{base_url}/a2a/review"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-old"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(retired, StatusCode::NOT_FOUND).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_cli_uses_admin_endpoint() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "reload-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    write_file(
        temp.path(),
        "harn.toml",
        &a2a_manifest(None).replace("/a2a/review", "/a2a/review-cli"),
    );

    let output = harn_command()
        .current_dir(temp.path())
        .arg("orchestrator")
        .arg("reload")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .envs(envs)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("reload ok"),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );

    let client = reqwest::Client::new();
    let mut auth_headers = json_headers();
    auth_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer reload-key"));
    let updated = client
        .post(format!("{base_url}/a2a/review-cli"))
        .headers(auth_headers)
        .body(br#"{"kind":"a2a.task.received","task":{"id":"task-cli"}}"#.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(updated, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_with_supplied_cert_and_key() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    write_bytes(temp.path(), "tls/cert.pem", cert_pem.as_bytes());
    write_bytes(temp.path(), "tls/key.pem", key_pem.as_bytes());

    let secret = "tls-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ];
    let args = ["--cert", "tls/cert.pem", "--key", "tls/key.pem"];
    let mut process = spawn_orchestrator(&temp, &args, &envs);
    let base_url = process.wait_for_listener_url();
    assert!(base_url.starts_with("https://"), "{base_url}");

    let body = br#"{"action":"opened","issue":{"number":2}}"#;
    let response = reqwest::Client::builder()
        .add_root_certificate(Certificate::from_pem(cert_pem.as_bytes()).unwrap())
        .build()
        .unwrap()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = process
        .child
        .wait_timeout(PROCESS_FAIL_FAST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"));
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn disallowed_origin_is_rejected() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        &base_manifest(Some(
            r#"[orchestrator]
allowed_origins = ["https://allowed.example"]"#,
        )),
    );
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "origin-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":3}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(
            secret,
            body,
            Some("https://blocked.example"),
        ))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::FORBIDDEN).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_body_is_rejected() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "body-limit-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = vec![b'a'; (10 * 1024 * 1024) + 1];
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, &body, None))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::PAYLOAD_TOO_LARGE).await;

    send_sigterm(&mut process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_in_flight_request() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let request_entered_path = temp.path().join("request-entered");
    let request_release_path = temp.path().join("request-release");
    let request_entered_value = request_entered_path.to_string_lossy().into_owned();
    let request_release_value = request_release_path.to_string_lossy().into_owned();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "shutdown-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        (
            "HARN_ORCHESTRATOR_TEST_REQUEST_ENTERED_FILE",
            request_entered_value.as_str(),
        ),
        (
            "HARN_ORCHESTRATOR_TEST_REQUEST_RELEASE_FILE",
            request_release_value.as_str(),
        ),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":4}}"#.to_vec();
    let request = tokio::spawn({
        let client = reqwest::Client::new();
        let url = format!("{base_url}/triggers/github-new-issue");
        let headers = github_headers(secret, &body, None);
        async move { client.post(url).headers(headers).body(body).send().await }
    });

    wait_for_path(&request_entered_path, EVENT_FAIL_FAST_TIMEOUT);
    send_sigterm(&mut process.child);
    fs::write(&request_release_path, b"release").unwrap();
    let response = request.await.unwrap().unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"in_flight\": 0"), "snapshot={snapshot}");
}

#[tokio::test(flavor = "multi_thread")]
async fn json_log_format_writes_structured_rotating_file_with_trace_ids() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "json-log-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--log-format", "json"], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":5}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");

    let log_path = temp.path().join("state/logs/orchestrator.log");
    let log = fs::read_to_string(&log_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", log_path.display()));
    let records: Vec<JsonValue> = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect();
    assert!(
        records.iter().any(|record| record
            .get("message")
            .and_then(JsonValue::as_str)
            .is_some_and(|message| message == "trigger event accepted")),
        "log={log}"
    );
    assert!(
        records.iter().all(|record| record.get("trace_id").is_some()
            || record
                .get("fields")
                .and_then(|fields| fields.get("trace_id"))
                .is_some()),
        "log={log}"
    );
}

// Regression coverage for harn#327 and harn#479: ingest should inject W3C
// trace-context headers, queue append should preserve the trace, and dispatch
// should adopt the queue append span as its remote parent.
//
// The orchestrator subprocess runs with the simple span processor
// (`HARN_OTEL_SPAN_PROCESSOR=simple`). That replaces the production batch
// pipeline with a synchronous "export each span on close" pipeline. The test
// waits for the existing pump lifecycle event before shutdown, so it asserts
// trace propagation after the dispatch span has actually closed instead of
// racing the ack-first inbox pump.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otel_exports_ingest_and_dispatch_spans_with_shared_trace_id() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let collector = MockOtelCollector::start();
    let secret = "otel-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("HARN_OTEL_ENDPOINT", collector.url.as_str()),
        ("HARN_OTEL_SERVICE_NAME", "harn-orchestrator-test"),
        (
            "HARN_OTEL_HEADERS",
            "authorization=Bearer otel-token,x-tenant-id=tenant-abc",
        ),
        // Synchronous export per span close. See doc comment above this fn.
        ("HARN_OTEL_SPAN_PROCESSOR", "simple"),
        ("RUST_LOG", "info"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":5}}"#;
    let client = reqwest::Client::new();
    let request = client
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .build()
        .unwrap();
    let response = client.execute(request).await.unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    let spans = loop {
        let spans = collector.collected_spans();
        let has_ingest = spans.iter().any(|span| span.name == "ingest");
        let has_queue_append = spans.iter().any(|span| span.name == "queue_append");
        let has_dispatch = spans.iter().any(|span| span.name == "dispatch");
        if has_ingest && has_queue_append && has_dispatch {
            break spans;
        }
        if Instant::now() >= deadline {
            let requests = collector.requests.lock().unwrap().clone();
            panic!(
                "timed out waiting for OTel spans\ncollector_headers={:#?}",
                requests
                    .iter()
                    .map(|request| request.headers.clone())
                    .collect::<Vec<_>>()
            );
        }
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    };

    let ingest = spans.iter().find(|span| span.name == "ingest").unwrap();
    let queue_append = spans
        .iter()
        .find(|span| span.name == "queue_append")
        .unwrap();
    let dispatch = spans.iter().find(|span| span.name == "dispatch").unwrap();
    assert_eq!(ingest.trace_id, queue_append.trace_id);
    assert_eq!(ingest.trace_id, dispatch.trace_id);
    assert_ne!(ingest.span_id, queue_append.span_id);
    assert_ne!(ingest.span_id, dispatch.span_id);
    assert!(queue_append.parent_span_id.is_some());
    assert_eq!(
        dispatch.parent_span_id.as_deref(),
        Some(queue_append.span_id.as_str())
    );

    let ingest_trace_id = attribute_string(ingest, "trace_id").unwrap();
    let dispatch_trace_id = attribute_string(dispatch, "trace_id").unwrap();
    assert_eq!(ingest_trace_id, dispatch_trace_id);
    assert_eq!(
        attribute_string(dispatch, "result.status").as_deref(),
        Some("succeeded")
    );
    assert!(
        attribute_string(dispatch, "result.duration_ms").is_some(),
        "dispatch span was missing duration attribute: {dispatch:?}"
    );

    let requests = collector.requests.lock().unwrap().clone();
    assert!(
        requests.iter().any(|request| {
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer otel-token")
        }),
        "collector never saw Authorization header: {requests:?}"
    );
    assert!(
        requests.iter().any(|request| {
            request
                .headers
                .iter()
                .any(|(name, value)| name == "x-tenant-id" && value == "tenant-abc")
        }),
        "collector never saw tenant header: {requests:?}"
    );
}
