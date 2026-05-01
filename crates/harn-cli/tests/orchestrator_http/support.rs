#[path = "../support/mod.rs"]
mod shared_support;

pub(super) use std::fs;
pub(super) use std::io::{BufRead, BufReader};
pub(super) use std::path::Path;
pub(super) use std::process::Stdio;
pub(super) use std::sync::mpsc::{self, Receiver};
pub(super) use std::thread;
pub(super) use std::time::Instant;

pub(super) use crate::test_util::process::harn_command;
pub(super) use crate::test_util::timing::{
    self, ChildExitWatcher, EVENT_FAIL_FAST_TIMEOUT, LOG_RECV_POLL_INTERVAL,
    PROCESS_FAIL_FAST_TIMEOUT,
};
pub(super) use harn_vm::event_log::{EventLog, SqliteEventLog, Topic};
pub(super) use hmac::{Hmac, KeyInit, Mac};
pub(super) use rcgen::generate_simple_self_signed;
pub(super) use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
pub(super) use reqwest::Certificate;
pub(super) use reqwest::StatusCode;
pub(super) use serde_json::Value as JsonValue;
pub(super) use sha2::Sha256;
pub(super) use tempfile::TempDir;
pub(super) use time::OffsetDateTime;

const STARTUP_PREFIX: &str = "[harn] HTTP listener ready on ";
const STARTUP_NEEDLE: &str = "HTTP listener ready";
pub(super) const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";
type HmacSha256 = Hmac<Sha256>;

pub(super) fn lock_orchestrator_tests() -> shared_support::OrchestratorProcessTestLock {
    shared_support::lock_orchestrator_process_tests()
}

pub(super) fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

pub(super) fn write_bytes(dir: &Path, relative: &str, bytes: &[u8]) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

pub(super) fn base_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn github_harn_override_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#
}

pub(super) fn github_marker_handler_module(marker_path: &Path) -> String {
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

pub(super) fn slack_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn notion_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn echo_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn stream_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn slack_handler_module(marker_path: &Path) -> String {
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

pub(super) fn notion_handler_module(marker_path: &Path) -> String {
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

pub(super) fn echo_handler_module(marker_path: &Path) -> String {
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

pub(super) fn stream_handler_module(marker_path: &Path) -> String {
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

pub(super) fn echo_connector_module() -> &'static str {
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

pub(super) fn github_override_connector_module() -> &'static str {
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

pub(super) fn a2a_manifest(orchestrator_block: Option<&str>) -> String {
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

pub(super) fn a2a_handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) {
  log(event.kind)
}
"#
}

pub(super) fn spawn_orchestrator(
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

pub(super) struct OrchestratorProcess {
    pub(super) child: ChildExitWatcher,
    rx: Receiver<String>,
    handle: Option<thread::JoinHandle<String>>,
}

impl OrchestratorProcess {
    pub(super) fn wait_for_listener_url(&mut self) -> String {
        let deadline = Instant::now() + PROCESS_FAIL_FAST_TIMEOUT;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(LOG_RECV_POLL_INTERVAL) {
                Ok(line) if line.contains(STARTUP_NEEDLE) => {
                    if let Some(url) = listener_url_from_line(&line) {
                        shared_support::wait_for_readyz(
                            &mut self.child,
                            &url,
                            PROCESS_FAIL_FAST_TIMEOUT,
                        )
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

    pub(super) fn shutdown_and_join_stderr(&mut self) -> String {
        self.child.kill();
        let _ = self.child.wait_timeout(PROCESS_FAIL_FAST_TIMEOUT);
        self.join_stderr()
    }

    pub(super) fn join_stderr(&mut self) -> String {
        self.handle
            .take()
            .expect("stderr collector thread")
            .join()
            .expect("stderr collector result")
    }
}

pub(super) fn listener_url_from_line(line: &str) -> Option<String> {
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

pub(super) fn send_sigterm(child: &mut ChildExitWatcher) {
    child.terminate();
}

pub(super) fn wait_for_exit(child: &mut ChildExitWatcher) {
    child.wait_for_success(PROCESS_FAIL_FAST_TIMEOUT);
}

pub(super) async fn wait_for_exit_async(child: &mut ChildExitWatcher) -> std::process::ExitStatus {
    child
        .wait_timeout(PROCESS_FAIL_FAST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"))
}

pub(super) fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256={encoded}")
}

pub(super) fn slack_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    let mut encoded = String::new();
    for byte in mac.finalize().into_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("v0={encoded}")
}

pub(super) fn github_headers(secret: &str, body: &[u8], origin: Option<&str>) -> HeaderMap {
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

pub(super) fn slack_headers(secret: &str, timestamp: i64, body: &[u8]) -> HeaderMap {
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

pub(super) fn notion_headers(secret: &str, body: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "X-Notion-Signature",
        HeaderValue::from_str(&github_signature(secret, body)).unwrap(),
    );
    headers.insert("request-id", HeaderValue::from_static("req-notion-123"));
    headers
}

pub(super) fn state_snapshot(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap()
}

pub(super) async fn read_topic_events(
    temp: &TempDir,
    topic: &str,
) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
    let log = SqliteEventLog::open(temp.path().join("state/events.sqlite"), 32).unwrap();
    let topic = Topic::new(topic).unwrap();
    log.read_range(&topic, None, usize::MAX).await.unwrap()
}

pub(super) async fn wait_for_topic_event(
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

pub(super) async fn assert_status(response: reqwest::Response, expected: StatusCode) {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, expected, "status={status} body={body}");
}

pub(super) fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

pub(super) fn wait_for_path(path: &Path, timeout: std::time::Duration) {
    timing::wait_for_nonempty_file(path, timeout);
}

pub(super) fn wait_for_json_file(path: &Path, timeout: std::time::Duration) -> JsonValue {
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
