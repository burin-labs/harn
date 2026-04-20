#![cfg(unix)]
// Orchestrator HTTP tests serialize against a std::sync::Mutex to prevent
// parallel binds of the same port. Each test holds the guard across .await
// boundaries (spawn orchestrator, make requests, drain); that's the correct
// pattern for this serialization. The clippy lint against await-holding-lock
// is overly strict for this use case, so allow it at the module level.
#![allow(clippy::await_holding_lock)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use hmac::{Hmac, Mac};
use rcgen::generate_simple_self_signed;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
use reqwest::Certificate;
use reqwest::StatusCode;
use serde_json::Value as JsonValue;
use sha2::Sha256;
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const STARTUP_PREFIX: &str = "[harn] HTTP listener ready on ";
const STARTUP_NEEDLE: &str = "HTTP listener ready";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";

type HmacSha256 = Hmac<Sha256>;

static ORCHESTRATOR_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_orchestrator_tests() -> MutexGuard<'static, ()> {
    ORCHESTRATOR_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
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

fn handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#
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

fn slack_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_slack(event: TriggerEvent) {{
  sleep(4100ms)
  write_file({marker:?}, event.kind)
}}
"#,
        marker = marker_path.display().to_string()
    )
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_harn"));
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
        child,
        rx,
        handle: Some(handle),
    }
}

struct OrchestratorProcess {
    child: Child,
    rx: Receiver<String>,
    handle: Option<thread::JoinHandle<String>>,
}

impl OrchestratorProcess {
    fn wait_for_listener_url(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) if line.contains(STARTUP_NEEDLE) => {
                    if let Some(url) = listener_url_from_line(&line) {
                        return url;
                    }
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_wait().unwrap() {
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
        let _ = self.child.kill();
        let _ = self.child.wait();
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

fn send_sigterm(child: &Child) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill exited with {status}");
}

fn wait_for_exit(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "child exited unsuccessfully: {status}");
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
}

async fn wait_for_exit_async(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for orchestrator exit");
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

fn state_snapshot(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap()
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

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {}", path.display());
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

    send_sigterm(&process.child);
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
async fn slack_webhook_acknowledges_before_handler_finishes() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("slack-handler.txt");
    write_file(temp.path(), "harn.toml", &slack_manifest(None));
    write_file(temp.path(), "lib.harn", &slack_handler_module(&marker_path));

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

    let started = Instant::now();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/slack-mentions"))
        .headers(slack_headers(secret, timestamp, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "slack ack path took too long: {:?}",
        started.elapsed()
    );
    assert!(
        !marker_path.exists(),
        "handler should not have completed before the HTTP ack"
    );
    wait_for_path(&marker_path, Duration::from_secs(10));
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "app_mention");

    send_sigterm(&process.child);
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

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
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

    send_sigterm(&process.child);
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

    send_sigterm(&process.child);
    let status = process.child.wait().unwrap();
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

    send_sigterm(&process.child);
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

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_in_flight_request() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "shutdown-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("HARN_ORCHESTRATOR_TEST_REQUEST_DELAY_MS", "500"),
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

    tokio::time::sleep(Duration::from_millis(100)).await;
    send_sigterm(&process.child);
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

// Regression coverage for harn#327: ingest should inject W3C trace-context
// headers, and dispatch should adopt that remote parent so both spans share a
// trace ID with `dispatch.parent_span_id == ingest.span_id`.
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

    send_sigterm(&process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let deadline = Instant::now() + Duration::from_secs(10);
    let spans = loop {
        let spans = collector.collected_spans();
        let has_ingest = spans.iter().any(|span| span.name == "ingest");
        let has_dispatch = spans.iter().any(|span| span.name == "dispatch");
        if has_ingest && has_dispatch {
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let ingest = spans.iter().find(|span| span.name == "ingest").unwrap();
    let dispatch = spans.iter().find(|span| span.name == "dispatch").unwrap();
    assert_eq!(ingest.trace_id, dispatch.trace_id);
    assert_ne!(ingest.span_id, dispatch.span_id);
    assert_eq!(
        dispatch.parent_span_id.as_deref(),
        ingest.parent_span_id.as_deref()
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
