//! Shared helpers for orchestrator HTTP integration tests.
//!
//! Tests run the orchestrator in-process via [`OrchestratorHarness`] —
//! no subprocess, no SQLite polling. Event waits use the event-log
//! broadcast channel; lifecycle assertions read the state snapshot
//! that the harness writes during drain.

#![allow(dead_code)]

pub(super) use std::fs;
pub(super) use std::path::Path;
pub(super) use std::time::Duration;

pub(super) use futures::StreamExt;
pub(super) use harn_cli::commands::orchestrator::harness::{
    OrchestratorConfig, OrchestratorHarness,
};
pub(super) use harn_vm::event_log::{EventLog, LogEvent, Topic};
pub(super) use hmac::{Hmac, KeyInit, Mac};
pub(super) use rcgen::generate_simple_self_signed;
pub(super) use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
pub(super) use reqwest::Certificate;
pub(super) use reqwest::StatusCode;
pub(super) use serde_json::Value as JsonValue;
pub(super) use sha2::Sha256;
pub(super) use tempfile::TempDir;
pub(super) use time::OffsetDateTime;

pub(super) use crate::test_util::cli::write_file;
pub(super) use crate::test_util::connectors::github_connector_module;
use crate::test_util::connectors::provider_declarations;

type HmacSha256 = Hmac<Sha256>;

// ── Manifest and module fixtures ─────────────────────────────────────────────

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

"#
    .to_string();
    manifest.push_str(provider_declarations());
    manifest.push_str(
        r#"
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
    );
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

pub fn on_issue(harness: Harness, event: TriggerEvent) {
  harness.stdio.log(event.kind)
}
"#
}

pub(super) fn github_marker_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_issue(harness: Harness, event: TriggerEvent) {{
  harness.fs.write_text({marker:?}, event.kind)
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

"#
    .to_string();
    manifest.push_str(provider_declarations());
    manifest.push_str(
        r#"
[[triggers]]
id = "slack-mentions"
kind = "webhook"
provider = "slack"
match = { events = ["app_mention"] }
handler = "handlers::on_slack"
secrets = { signing_secret = "slack/signing-secret" }
"#,
    );
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

pub fn on_slack(harness: Harness, event: TriggerEvent) {{
  harness.fs.write_text({marker:?}, event.kind)
}}
"#,
        marker = marker_path.display().to_string()
    )
}

pub(super) fn echo_handler_module(marker_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_echo(harness: Harness, event: TriggerEvent) {{
  const ping = harness.net.connector_call("echo", "ping", {{
    message: event.provider_payload.raw.body.message,
  }})
  harness.fs.write_text({marker:?}, json_stringify({{
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

pub fn on_stream(harness: Harness, event: TriggerEvent) {{
  harness.fs.write_text({marker:?}, json_stringify({{
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
let active_bindings = []

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

pub fn init(harness: Harness, _ctx) {
  harness.obs.event_log_emit("connectors.echo.lifecycle", "init", {phase: "init"})
}

pub fn activate(harness: Harness, bindings) {
  active_bindings = bindings
  harness.obs.metrics_inc("echo_activate_bindings", len(bindings))
  harness.obs.event_log_emit("connectors.echo.lifecycle", "activate", {
    binding_count: len(bindings),
  })
}

pub fn shutdown(harness: Harness) {
  harness.obs.event_log_emit("connectors.echo.lifecycle", "shutdown", {
    binding_count: len(active_bindings),
  })
}

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  const token = harness.secrets.read("echo/api-token")
  harness.obs.metrics_inc("echo_normalize_calls")
  harness.obs.event_log_emit("connectors.echo.lifecycle", "normalize", {
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

pub fn call(harness: Harness, method, args) {
  if method == "ping" {
    harness.obs.metrics_inc("echo_client_calls")
    harness.obs.event_log_emit("connectors.echo.calls", "ping", {
      message: args.message,
    })
    return {
      message: args.message,
      token: harness.secrets.read("echo/api-token"),
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

pub fn init(harness: Harness, _ctx) {
  harness.obs.event_log_emit("connectors.github.override", "init", {provider: "github"})
}

pub fn activate(harness: Harness, bindings) {
  harness.obs.metrics_inc("github_override_activate_bindings", len(bindings))
}

pub fn normalize_inbound(harness: Harness, raw) {
  const body = raw.body_json ?? json_parse(raw.body_text)
  harness.obs.event_log_emit("connectors.github.override", "normalize", {
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

pub fn call(_harness: Harness, method, _args) {
  throw "method_not_found:" + method
}
"#
}

pub(super) fn slack_connector_module() -> &'static str {
    r#"
pub fn provider_id() {
  return "slack"
}

pub fn kinds() {
  return ["webhook"]
}

pub fn payload_schema() {
  return "SlackEventPayload"
}

pub fn normalize_inbound(harness: Harness, raw) {
  const decoded = base64_decode(raw.body_base64)
  const timestamp = raw.headers["X-Slack-Request-Timestamp"] ?? raw.headers["x-slack-request-timestamp"] ?? ""
  const signature = raw.headers["X-Slack-Signature"] ?? raw.headers["x-slack-signature"] ?? ""
  const secret = harness.secrets.read("slack/signing-secret")
  const expected = "v0=" + hmac_sha256(secret, "v0:" + timestamp + ":" + decoded)
  if !constant_time_eq(signature, expected) {
    return {
      type: "reject",
      reject: {
        status: 400,
        body: {error: "invalid_signature"},
      },
    }
  }

  const body = raw.body_json ?? json_parse(decoded)
  if body.type == "url_verification" {
    return {
      type: "immediate_response",
      immediate_response: {
        status: 200,
        headers: {"content-type": "text/plain; charset=utf-8"},
        body: body.challenge,
      },
    }
  }

  const event_type = body.event.type ?? "event_callback"
  const channel_type = body.event.channel_type ?? ""
  const kind = if event_type == "message" && channel_type != "" {
    event_type + "." + channel_type
  } else {
    event_type
  }
  return {
    type: "event",
    event: {
      kind: kind,
      dedupe_key: "slack:" + (body.event_id ?? sha256(decoded)),
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}

pub fn call(_harness: Harness, method, _args) {
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

pub fn on_task(harness: Harness, event: TriggerEvent) -> string {
  harness.stdio.log(event.kind)
  return event.kind
}
"#
}

// ── In-process harness helpers ───────────────────────────────────────────────

pub(super) use crate::test_util::orchestrator::{
    await_topic_event, json_headers, lock_env_with, read_topic_events, shutdown,
    EVENT_FAIL_FAST_TIMEOUT,
};

/// Test harnesses opt in to `public_metrics = true` so existing
/// scrape-the-/metrics-endpoint assertions keep working unmodified.
/// Production defaults gate `/metrics` behind the listener auth
/// policy; the orchestrator_cli `metrics_endpoint_is_auth_gated_by_default_*`
/// test covers the production path explicitly.
const PUBLIC_METRICS: bool = true;

pub(super) fn test_config(temp: &TempDir) -> OrchestratorConfig {
    crate::test_util::orchestrator::test_config(temp, PUBLIC_METRICS)
}

/// Start the harness with the default config under `temp`.
pub(super) async fn start_harness(temp: &TempDir) -> OrchestratorHarness {
    start_harness_with(temp, |config| config).await
}

/// Start the harness with a custom config builder.
pub(super) async fn start_harness_with(
    temp: &TempDir,
    customize: impl FnOnce(OrchestratorConfig) -> OrchestratorConfig,
) -> OrchestratorHarness {
    crate::test_util::orchestrator::start_harness_with(temp, PUBLIC_METRICS, customize).await
}

/// Wait until the orchestrator emits a `pump_dispatch_completed` lifecycle
/// event with `status: "completed"`. After this fires the handler has
/// finished, so reading any handler-written marker file is race-free.
pub(super) async fn await_pump_dispatch_completed(harness: &OrchestratorHarness) {
    await_topic_event(&harness.event_log(), "orchestrator.lifecycle", |event| {
        match pump_dispatch_status(event) {
            Some("completed") => true,
            Some("failed") => {
                panic!("pump dispatch failed: {}", event.payload);
            }
            _ => false,
        }
    })
    .await;
}

/// Wait until the orchestrator emits a `pump_dispatch_completed` event whose
/// `dispatched` count reaches `count`. Useful when several events feed the
/// same pump and you need to wait for the Nth to drain.
pub(super) async fn await_pump_dispatch_count(harness: &OrchestratorHarness, count: u64) {
    let topic = Topic::new("orchestrator.lifecycle").unwrap();
    let mut stream = harness
        .event_log()
        .subscribe(&topic, None)
        .await
        .expect("subscribe lifecycle");
    let mut total: u64 = 0;
    tokio::time::timeout(EVENT_FAIL_FAST_TIMEOUT, async {
        loop {
            let (_, event) = stream
                .next()
                .await
                .expect("lifecycle stream ended")
                .expect("lifecycle stream error");
            match pump_dispatch_status(&event) {
                Some("completed") => {
                    let dispatched = event.payload["dispatched"].as_u64().unwrap_or(0);
                    total = total.saturating_add(dispatched);
                    if total >= count {
                        return;
                    }
                }
                Some("failed") => {
                    panic!(
                        "pump dispatch failed before reaching {count}: {}",
                        event.payload
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {count} pump dispatches; got {total}"));
}

fn pump_dispatch_status(event: &LogEvent) -> Option<&str> {
    if event.kind != "pump_dispatch_completed" {
        return None;
    }
    event.payload.get("status").and_then(JsonValue::as_str)
}

/// Wait until `path` exists, polling at the approved cadence. The
/// orchestrator emits filesystem markers via the
/// `HARN_ORCHESTRATOR_TEST_*_FILE` env hooks for tests that need to
/// gate on handler-side state for which there is no lifecycle event
/// (e.g. a request has entered the handler body). Wrapping the poll
/// in [`tokio::time::timeout`] gives the same fail-fast ceiling as
/// [`await_topic_event`] without the wall-clock deadline pattern the
/// lint forbids.
pub(super) async fn wait_for_path(path: &Path) {
    tokio::time::timeout(EVENT_FAIL_FAST_TIMEOUT, async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for path {}", path.display()));
}

/// Read the on-disk state snapshot. The harness writes this file at
/// startup, during drain, and on shutdown — so callers should either
/// shut down the harness first or wait for the relevant lifecycle
/// event before reading.
pub(super) fn state_snapshot(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap()
}

/// Read the state snapshot from the harness's `state_dir` while the
/// harness is still running (e.g. after a `draining` event).
pub(super) fn state_snapshot_at(state_dir: &Path) -> String {
    fs::read_to_string(state_dir.join("orchestrator-state.json")).unwrap()
}

/// Fetch the Prometheus metrics exposition from the harness's
/// `/metrics` endpoint as plain text.
pub(super) async fn fetch_metrics(harness: &OrchestratorHarness) -> String {
    reqwest::Client::new()
        .get(format!("{}/metrics", harness.listener_url()))
        .send()
        .await
        .expect("metrics request")
        .text()
        .await
        .expect("metrics body")
}

// ── Signing / header helpers ─────────────────────────────────────────────────

pub(super) async fn assert_status(response: reqwest::Response, expected: StatusCode) {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, expected, "status={status} body={body}");
}

fn hex_encode(bytes: impl IntoIterator<Item = u8>) -> String {
    let mut encoded = String::new();
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

pub(super) fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex_encode(mac.finalize().into_bytes()))
}

pub(super) fn slack_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    format!("v0={}", hex_encode(mac.finalize().into_bytes()))
}

pub(super) fn github_headers(secret: &str, body: &[u8], origin: Option<&str>) -> HeaderMap {
    let mut headers = json_headers();
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
    let mut headers = json_headers();
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
