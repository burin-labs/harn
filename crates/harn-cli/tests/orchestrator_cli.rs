#![cfg(unix)]

mod support;
mod test_util;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::event_log::{
    ConsumerId, EventLog, EventLogBackendKind, EventLogConfig, LogEvent, Topic,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tempfile::TempDir;
use test_util::timing::{
    self, ChildExitWatcher, EVENT_FAIL_FAST_TIMEOUT, LOG_RECV_POLL_INTERVAL,
    PROCESS_FAIL_FAST_TIMEOUT,
};

const STARTUP_NEEDLE: &str = "HTTP listener ready on";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";
fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn gated_task_handler_module(release_path: &Path) -> String {
    format!(
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {{
  while !file_exists({release:?}) {{
    sleep(1ms)
  }}
  return event.kind
}}
"#,
        release = release_path.display().to_string()
    )
}

fn spawn_orchestrator(
    temp: &TempDir,
) -> (
    ChildExitWatcher,
    Receiver<String>,
    thread::JoinHandle<String>,
) {
    spawn_orchestrator_with(temp, &[], &[])
}

fn spawn_orchestrator_with(
    temp: &TempDir,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> (
    ChildExitWatcher,
    Receiver<String>,
    thread::JoinHandle<String>,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"));
    child
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
        child.arg(arg);
    }
    for (key, value) in envs {
        child.env(key, value);
    }
    let mut child = child.spawn().unwrap();

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

    (ChildExitWatcher::new(child), rx, handle)
}

fn wait_for_log_line(child: &mut ChildExitWatcher, rx: &Receiver<String>, needle: &str) {
    let deadline = Instant::now() + PROCESS_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        match rx.recv_timeout(LOG_RECV_POLL_INTERVAL) {
            Ok(line) if line.contains(needle) => return,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_status().unwrap() {
                    panic!("process exited before '{needle}' appeared: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("stderr stream closed before '{needle}' appeared");
            }
        }
    }
    panic!("timed out waiting for '{needle}'");
}

fn wait_for_listener_url(child: &mut ChildExitWatcher, rx: &Receiver<String>) -> String {
    let deadline = Instant::now() + PROCESS_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        match rx.recv_timeout(LOG_RECV_POLL_INTERVAL) {
            Ok(line) if line.contains(STARTUP_NEEDLE) => {
                let url = line
                    .split(STARTUP_NEEDLE)
                    .nth(1)
                    .expect("startup URL suffix")
                    .trim()
                    .to_string();
                support::wait_for_readyz(child, &url, PROCESS_FAIL_FAST_TIMEOUT)
                    .unwrap_or_else(|error| panic!("{error}"));
                return url;
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_status().unwrap() {
                    panic!("process exited before listener became ready: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("stderr stream closed before listener became ready");
            }
        }
    }
    child.kill();
    panic!("timed out waiting for listener URL");
}

fn send_sigterm(child: &mut ChildExitWatcher) {
    child.terminate();
}

fn wait_for_exit_code(child: &mut ChildExitWatcher, expected: i32) {
    child.wait_for_code(PROCESS_FAIL_FAST_TIMEOUT, expected);
}

fn wait_for_exit(child: &mut ChildExitWatcher) {
    child.wait_for_success(PROCESS_FAIL_FAST_TIMEOUT);
}

fn wait_for_any_exit(child: &mut ChildExitWatcher) {
    child
        .wait_timeout(PROCESS_FAIL_FAST_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"));
}

fn wait_for_path(path: &Path, timeout: std::time::Duration) {
    timing::wait_for_existing_path(path, timeout);
}

fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

fn bearer_headers() -> HeaderMap {
    let mut headers = json_headers();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key"));
    headers
}

async fn wait_for_metrics_contains(
    client: &reqwest::Client,
    base_url: &str,
    needles: &[&str],
) -> String {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = client
            .get(format!("{base_url}/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if needles.iter().all(|needle| last.contains(needle)) {
            return last;
        }
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    }
    panic!("timed out waiting for metrics samples {needles:?}; last={last}");
}

fn run_harn_with_env(temp: &TempDir, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harn"));
    command.current_dir(temp.path()).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read_topic_events(
    state_dir: &Path,
    topic_name: &str,
) -> Vec<(u64, harn_vm::event_log::LogEvent)> {
    let mut config = EventLogConfig::for_base_dir(state_dir).unwrap();
    let file_dir = state_dir.join("events");
    if file_dir.join("topics").is_dir() {
        config.backend = EventLogBackendKind::File;
        config.file_dir = file_dir;
    }
    let log = harn_vm::event_log::open_event_log(&config).unwrap();
    let topic = Topic::new(topic_name).unwrap();
    futures::executor::block_on(log.read_range(&topic, None, usize::MAX)).unwrap()
}

fn open_state_event_log(state_dir: &Path) -> Arc<harn_vm::event_log::AnyEventLog> {
    let mut config = EventLogConfig::for_base_dir(state_dir).unwrap();
    let file_dir = state_dir.join("events");
    if file_dir.join("topics").is_dir() {
        config.backend = EventLogBackendKind::File;
        config.file_dir = file_dir;
    }
    harn_vm::event_log::open_event_log(&config).unwrap()
}

async fn wait_for_consumer_cursor(
    state_dir: &Path,
    topic_name: &str,
    consumer: &str,
    at_least: u64,
) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    let log = open_state_event_log(state_dir);
    let topic = Topic::new(topic_name).unwrap();
    let consumer = ConsumerId::new(consumer).unwrap();
    while Instant::now() < deadline {
        let cursor = log
            .consumer_cursor(&topic, &consumer)
            .await
            .unwrap()
            .unwrap_or(0);
        if cursor >= at_least {
            return;
        }
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    }
    panic!("timed out waiting for consumer cursor {consumer} on {topic_name} to reach {at_least}");
}

fn seed_legacy_inbox_records(temp: &TempDir) {
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let config = EventLogConfig::for_base_dir(&state_dir).unwrap();
    let log = harn_vm::event_log::open_event_log(&config).unwrap();

    let legacy_topic = Topic::new(harn_vm::TRIGGER_INBOX_LEGACY_TOPIC).unwrap();
    let future_expiry_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 60_000;
    futures::executor::block_on(log.append(
        &legacy_topic,
        LogEvent::new(
            "dedupe_claim",
            serde_json::json!({
                "binding_id": "github-new-issue",
                "dedupe_key": "delivery-123",
                "expires_at_ms": future_expiry_ms,
            }),
        ),
    ))
    .unwrap();

    let event = harn_vm::TriggerEvent::new(
        harn_vm::ProviderId::from("webhook"),
        "webhook.received",
        None,
        "delivery-123",
        None,
        BTreeMap::new(),
        harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Webhook(
            harn_vm::triggers::GenericWebhookPayload {
                source: Some("legacy-fixture".to_string()),
                content_type: Some("application/json".to_string()),
                raw: serde_json::json!({"legacy": true}),
            },
        )),
        harn_vm::SignatureStatus::Unsigned,
    );
    futures::executor::block_on(
        log.append(
            &legacy_topic,
            LogEvent::new(
                "event_ingested",
                serde_json::to_value(harn_vm::triggers::dispatcher::InboxEnvelope {
                    trigger_id: Some("github-new-issue".to_string()),
                    binding_version: Some(1),
                    event,
                })
                .unwrap(),
            ),
        ),
    )
    .unwrap();
    futures::executor::block_on(log.flush()).unwrap();
}

fn wait_for_topic_kind(state_dir: &Path, topic_name: &str, kind: &str) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        if read_topic_events(state_dir, topic_name)
            .iter()
            .any(|(_, event)| event.kind == kind)
        {
            return;
        }
        timing::sleep_blocking(timing::RETRY_POLL_INTERVAL);
    }
    panic!("timed out waiting for {topic_name}/{kind}");
}

fn wait_for_topic_event(state_dir: &Path, topic_name: &str, predicate: impl Fn(&LogEvent) -> bool) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        if read_topic_events(state_dir, topic_name)
            .iter()
            .any(|(_, event)| predicate(event))
        {
            return;
        }
        timing::sleep_blocking(timing::RETRY_POLL_INTERVAL);
    }
    panic!("timed out waiting for matching {topic_name} event");
}

fn wait_for_topic_event_count(state_dir: &Path, topic_name: &str, kind: &str, expected: usize) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        let count = read_topic_events(state_dir, topic_name)
            .iter()
            .filter(|(_, event)| event.kind == kind)
            .count();
        if count >= expected {
            return;
        }
        timing::sleep_blocking(timing::RETRY_POLL_INTERVAL);
    }
    panic!("timed out waiting for {topic_name}/{kind} count {expected}");
}

fn sqlite_event_count(state_dir: &Path, topic_name: &str, kind: &str) -> usize {
    let output = Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import pathlib
import sqlite3
import sys

state_dir, topic, kind = sys.argv[1:]
conn = sqlite3.connect(str(pathlib.Path(state_dir) / "events.sqlite"))
count = conn.execute(
    "SELECT COUNT(*) FROM events WHERE topic = ? AND kind = ?",
    (topic, kind),
).fetchone()[0]
print(count)
"#,
        )
        .arg(state_dir)
        .arg(topic_name)
        .arg(kind)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "python stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

fn wait_for_sqlite_event_count(state_dir: &Path, topic_name: &str, kind: &str, expected: usize) {
    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while Instant::now() < deadline {
        if sqlite_event_count(state_dir, topic_name, kind) >= expected {
            return;
        }
        timing::sleep_blocking(timing::RETRY_POLL_INTERVAL);
    }
    panic!("timed out waiting for {topic_name}/{kind} count {expected}");
}

#[test]
fn orchestrator_serve_starts_and_shuts_down_cleanly() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
    );

    let (mut child, rx, handle) = spawn_orchestrator(&temp);
    wait_for_log_line(&mut child, &rx, STARTUP_NEEDLE);
    send_sigterm(&mut child);
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(stderr.contains("secret providers"), "stderr={stderr}");
    assert!(
        stderr.contains("registered triggers (1):"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("registered connectors (1):"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = temp.path().join("state/orchestrator-state.json");
    let snapshot_contents = fs::read_to_string(&snapshot).unwrap();
    assert!(snapshot_contents.contains("\"status\": \"stopped\""));
    assert!(snapshot_contents.contains("\"bind\": \"127.0.0.1:"));
}

// Regression coverage for harn#325: graceful shutdown should let an in-flight
// a2a-push dispatch finish within the configured shutdown window and emit the
// terminal `dispatch_succeeded` lifecycle event instead of a shutdown failure.
#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_drains_in_flight_dispatch_and_emits_lifecycle_events() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    let handler_release_path = temp.path().join("release-handler");
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        &gated_task_handler_module(&handler_release_path),
    );

    let envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
    ];
    let (mut child, rx, handle) =
        spawn_orchestrator_with(&temp, &["--shutdown-timeout", "5"], &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-123"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    wait_for_topic_kind(&state_dir, "triggers.lifecycle", "DispatchStarted");
    send_sigterm(&mut child);
    fs::write(&handler_release_path, b"release").unwrap();
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let lifecycle = read_topic_events(&state_dir, "orchestrator.lifecycle");
    assert!(lifecycle.iter().any(|(_, event)| event.kind == "draining"));
    assert!(lifecycle.iter().any(|(_, event)| {
        event.kind == "stopped" && event.payload["timed_out"] == serde_json::json!(false)
    }));

    let inbox = read_topic_events(&state_dir, harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC);
    assert!(
        inbox
            .iter()
            .any(|(_, event)| event.kind == "event_ingested"),
        "inbox={inbox:?}"
    );
    let legacy_inbox = read_topic_events(&state_dir, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC);
    assert!(legacy_inbox.is_empty(), "legacy_inbox={legacy_inbox:?}");

    let outbox = read_topic_events(&state_dir, "trigger.outbox");
    assert!(outbox.iter().any(|(_, event)| {
        event.kind == "dispatch_succeeded" && event.payload["result"] == serde_json::json!("push")
    }));

    let snapshot_contents =
        fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap();
    assert!(snapshot_contents.contains("\"status\": \"stopped\""));
    assert!(
        snapshot_contents.contains("\"in_flight\": 0"),
        "snapshot={snapshot_contents}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_continues_after_pump_error_and_persists_stopped_state() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );

    let envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_TEST_ORCHESTRATOR_FAIL_PENDING_PUMP", "1"),
    ];
    let (mut child, rx, handle) =
        spawn_orchestrator_with(&temp, &["--shutdown-timeout", "5"], &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-240"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    send_sigterm(&mut child);
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(
        stderr.contains("pump drain error for orchestrator.triggers.pending"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let lifecycle = read_topic_events(&state_dir, "orchestrator.lifecycle");
    assert!(
        lifecycle.iter().any(|(_, event)| event.kind == "stopped"),
        "lifecycle={lifecycle:?}"
    );

    let snapshot_contents =
        fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap();
    assert!(snapshot_contents.contains("\"status\": \"stopped\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_spawned_inbox_dispatch_tasks() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    let inbox_release_file = temp.path().join("release-inbox-dispatch");
    let inbox_release_value = inbox_release_file.to_string_lossy().into_owned();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );

    let envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            inbox_release_value.as_str(),
        ),
    ];
    let (mut child, rx, handle) =
        spawn_orchestrator_with(&temp, &["--shutdown-timeout", "5"], &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-241"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    wait_for_topic_event(&state_dir, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    });
    wait_for_topic_event(&state_dir, "orchestrator.lifecycle", |event| {
        event.kind == "pump_acked" && event.payload["event_log_id"] == serde_json::json!(1)
    });

    send_sigterm(&mut child);
    fs::write(&inbox_release_file, b"release").unwrap();
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let outbox = read_topic_events(&state_dir, "trigger.outbox");
    assert!(
        outbox
            .iter()
            .any(|(_, event)| event.kind == "dispatch_succeeded"),
        "outbox={outbox:?}"
    );
    let lifecycle = read_topic_events(&state_dir, "orchestrator.lifecycle");
    assert!(
        lifecycle.iter().any(|(_, event)| {
            event.kind == "pump_dispatch_completed"
                && event.payload["event_log_id"] == serde_json::json!(1)
                && event.payload["status"] == serde_json::json!("completed")
        }),
        "lifecycle={lifecycle:?}"
    );

    let snapshot_contents =
        fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap();
    assert!(
        snapshot_contents.contains("\"in_flight\": 0"),
        "snapshot={snapshot_contents}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inbox_pump_backpressures_before_ack_when_outstanding_limit_is_full() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    let inbox_release_file = temp.path().join("release-inbox-dispatch");
    let inbox_release_value = inbox_release_file.to_string_lossy().into_owned();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );

    let envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            inbox_release_value.as_str(),
        ),
    ];
    let extra_args = ["--shutdown-timeout", "5", "--pump-max-outstanding", "1"];
    let (mut child, rx, handle) = spawn_orchestrator_with(&temp, &extra_args, &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");
    let client = reqwest::Client::new();

    for id in ["task-478-a", "task-478-b"] {
        let body = serde_json::json!({
            "kind": "a2a.task.received",
            "task": {"id": id},
        });
        let response = client
            .post(format!("{base_url}/a2a/review"))
            .headers(bearer_headers())
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    wait_for_consumer_cursor(
        &state_dir,
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        &format!(
            "orchestrator-pump.{}",
            harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC
        ),
        1,
    )
    .await;
    wait_for_topic_event(&state_dir, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    });
    wait_for_topic_event_count(
        &state_dir,
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        "event_ingested",
        2,
    );

    let log = open_state_event_log(&state_dir);
    let topic = Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap();
    let consumer = ConsumerId::new(format!(
        "orchestrator-pump.{}",
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC
    ))
    .unwrap();
    let cursor = log.consumer_cursor(&topic, &consumer).await.unwrap();
    assert_eq!(
        cursor,
        Some(1),
        "second inbox event was acked before admission"
    );

    let _metrics = wait_for_metrics_contains(
        &client,
        &base_url,
        &[
            "harn_orchestrator_pump_outstanding{topic=\"trigger.inbox.envelopes\"} 1",
            "harn_orchestrator_pump_backlog{topic=\"trigger.inbox.envelopes\"} 1",
        ],
    )
    .await;

    send_sigterm(&mut child);
    fs::write(&inbox_release_file, b"release").unwrap();
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let outbox = read_topic_events(&state_dir, "trigger.outbox");
    assert_eq!(
        outbox
            .iter()
            .filter(|(_, event)| event.kind == "dispatch_succeeded")
            .count(),
        2,
        "outbox={outbox:?}"
    );
    let lifecycle = read_topic_events(&state_dir, "orchestrator.lifecycle");
    for kind in [
        "pump_received",
        "pump_eligible",
        "pump_admitted",
        "pump_dispatch_started",
        "pump_dispatch_completed",
        "pump_acked",
    ] {
        assert!(
            lifecycle.iter().any(|(_, event)| event.kind == kind),
            "missing {kind}: lifecycle={lifecycle:?}"
        );
    }
}

#[test]
fn orchestrator_queue_soft_migrates_legacy_inbox_topics() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_event"
secrets = { signing_secret = "github/webhook-secret" }
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_event(event: TriggerEvent) {
  log(event.kind)
}
"#,
    );
    seed_legacy_inbox_records(&temp);
    let state_dir = temp.path().join("state");
    let legacy_before = read_topic_events(&state_dir, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC);
    assert_eq!(legacy_before.len(), 2, "legacy_before={legacy_before:?}");

    let (mut child, rx, handle) = spawn_orchestrator(&temp);
    wait_for_log_line(&mut child, &rx, STARTUP_NEEDLE);
    child.kill();
    wait_for_any_exit(&mut child);
    let _stderr = handle.join().expect("stderr collector thread");
    let legacy_after = read_topic_events(&state_dir, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC);
    assert_eq!(legacy_after.len(), 2, "legacy_after={legacy_after:?}");
    assert_eq!(
        legacy_after
            .iter()
            .filter(|(_, event)| event.kind == "dedupe_claim")
            .count(),
        1,
        "legacy_after={legacy_after:?}"
    );
    assert!(
        legacy_after
            .iter()
            .any(|(_, event)| event.kind == "event_ingested"),
        "legacy_after={legacy_after:?}"
    );

    let config = EventLogConfig::for_base_dir(&state_dir).unwrap();
    let log = harn_vm::event_log::open_event_log(&config).unwrap();
    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let inbox =
        futures::executor::block_on(harn_vm::InboxIndex::new(log.clone(), metrics)).unwrap();
    assert!(!futures::executor::block_on(inbox.insert_if_new(
        "github-new-issue",
        "delivery-123",
        Duration::from_secs(60),
    ))
    .unwrap());
}

// Regression coverage for harn#328: a bounded drain should truncate backlog on
// shutdown, persist each pump's consumer cursor in the event log, and let the
// next orchestrator run replay the remaining backlog to completion.
#[tokio::test(flavor = "multi_thread")]
async fn bounded_pump_drain_truncates_and_replays_remaining_backlog_after_restart() {
    let _lock = support::lock_orchestrator_process_tests();
    const TOTAL_EVENTS: usize = 60;

    let temp = TempDir::new().unwrap();
    let pump_release_file = temp.path().join("release-pending-pump");
    let pump_waiting_file = temp.path().join("pending-pump-waiting");
    let pump_draining_file = temp.path().join("pending-pump-draining");
    let pump_release_value = pump_release_file.to_string_lossy().into_owned();
    let pump_waiting_value = pump_waiting_file.to_string_lossy().into_owned();
    let pump_draining_value = pump_draining_file.to_string_lossy().into_owned();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );

    let envs = [
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_EVENT_LOG_QUEUE_DEPTH", "8192"),
        (
            "HARN_TEST_ORCHESTRATOR_PUMP_RELEASE_FILE",
            pump_release_value.as_str(),
        ),
        (
            "HARN_TEST_ORCHESTRATOR_PUMP_WAITING_FILE",
            pump_waiting_value.as_str(),
        ),
        (
            "HARN_TEST_ORCHESTRATOR_PUMP_DRAINING_FILE",
            pump_draining_value.as_str(),
        ),
    ];
    let extra_args = [
        "--shutdown-timeout",
        "5",
        "--drain-max-items",
        "10",
        "--drain-deadline",
        "1",
    ];
    let (mut child, rx, handle) = spawn_orchestrator_with(&temp, &extra_args, &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");

    let client = reqwest::Client::new();
    for index in 0..TOTAL_EVENTS {
        let body = serde_json::json!({
            "kind": "a2a.task.received",
            "task": {"id": format!("task-{index}")},
        });
        let response = client
            .post(format!("{base_url}/a2a/review"))
            .headers(bearer_headers())
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    wait_for_path(&pump_waiting_file, EVENT_FAIL_FAST_TIMEOUT);
    send_sigterm(&mut child);
    wait_for_path(&pump_draining_file, EVENT_FAIL_FAST_TIMEOUT);
    fs::write(&pump_release_file, b"release").unwrap();
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
    assert!(stderr.contains("pump drain truncated"), "stderr={stderr}");

    let restart_envs = [
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_EVENT_LOG_QUEUE_DEPTH", "8192"),
    ];
    let (mut restart_child, restart_rx, restart_handle) =
        spawn_orchestrator_with(&temp, &extra_args, &restart_envs);
    wait_for_listener_url(&mut restart_child, &restart_rx);
    wait_for_sqlite_event_count(
        &state_dir,
        "trigger.outbox",
        "dispatch_succeeded",
        TOTAL_EVENTS,
    );
    send_sigterm(&mut restart_child);
    wait_for_exit(&mut restart_child);
    let restart_stderr = restart_handle.join().expect("stderr collector thread");
    assert!(
        restart_stderr.contains(SHUTDOWN_NEEDLE),
        "stderr={restart_stderr}"
    );

    assert_eq!(
        sqlite_event_count(&state_dir, "trigger.outbox", "dispatch_succeeded"),
        TOTAL_EVENTS
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_surfaces_stranded_envelopes_and_recover_replays_them_explicitly() {
    let _lock = support::lock_orchestrator_process_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
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
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );

    let inbox_release_file = temp.path().join("release-inbox-dispatch");
    let inbox_release_file = inbox_release_file.to_string_lossy().into_owned();
    let envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_TEST_DISPATCHER_FAIL_BEFORE_OUTBOX", "1"),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            inbox_release_file.as_str(),
        ),
    ];
    let (mut crashing_child, crashing_rx, crashing_handle) =
        spawn_orchestrator_with(&temp, &[], &envs);
    let base_url = wait_for_listener_url(&mut crashing_child, &crashing_rx);

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-242"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await;
    fs::write(&inbox_release_file, b"release").unwrap();
    let response = response.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    wait_for_exit_code(&mut crashing_child, 86);
    let crashing_stderr = crashing_handle.join().expect("stderr collector thread");
    assert!(
        crashing_stderr.contains("registered connectors (1): a2a-push"),
        "stderr={crashing_stderr}"
    );

    let restart_envs = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
    ];
    let (mut restarted_child, restarted_rx, restarted_handle) =
        spawn_orchestrator_with(&temp, &[], &restart_envs);
    wait_for_listener_url(&mut restarted_child, &restarted_rx);

    let state_dir = temp.path().join("state");
    wait_for_topic_event(&state_dir, "orchestrator.lifecycle", |event| {
        event.kind == "startup_stranded_envelopes" && event.payload["count"] == serde_json::json!(1)
    });
    let lifecycle = read_topic_events(&state_dir, "orchestrator.lifecycle");
    assert!(lifecycle.iter().any(|(_, event)| {
        event.kind == "startup_stranded_envelopes" && event.payload["count"] == serde_json::json!(1)
    }));

    let queue = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "queue",
            "--config",
            "harn.toml",
            "--state-dir",
            "./state",
        ],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    );
    assert!(
        queue.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        queue.status.code(),
        stdout(&queue),
        stderr(&queue)
    );
    assert!(stdout(&queue).contains("stranded_envelopes=1"));

    let recover_without_yes = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "recover",
            "--config",
            "harn.toml",
            "--state-dir",
            "./state",
            "--envelope-age",
            "0s",
        ],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    );
    assert!(!recover_without_yes.status.success());
    assert!(stderr(&recover_without_yes).contains("without --yes"));

    let dry_run = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "recover",
            "--config",
            "harn.toml",
            "--state-dir",
            "./state",
            "--envelope-age",
            "0s",
            "--dry-run",
        ],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    );
    assert!(
        dry_run.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        dry_run.status.code(),
        stdout(&dry_run),
        stderr(&dry_run)
    );
    assert!(stdout(&dry_run).contains("stranded_envelopes=1"));
    assert!(stdout(&dry_run).contains("event_id=trigger_evt_"));

    let recover = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "recover",
            "--config",
            "harn.toml",
            "--state-dir",
            "./state",
            "--envelope-age",
            "0s",
            "--yes",
        ],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    );
    assert!(
        recover.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        recover.status.code(),
        stdout(&recover),
        stderr(&recover)
    );
    assert!(stdout(&recover).contains("status=dispatched"));

    let queue_after = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "queue",
            "--config",
            "harn.toml",
            "--state-dir",
            "./state",
        ],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    );
    assert!(
        queue_after.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        queue_after.status.code(),
        stdout(&queue_after),
        stderr(&queue_after)
    );
    assert!(stdout(&queue_after).contains("stranded_envelopes=0"));

    send_sigterm(&mut restarted_child);
    wait_for_exit(&mut restarted_child);
    let restarted_stderr = restarted_handle.join().expect("stderr collector thread");
    assert!(
        restarted_stderr.contains(SHUTDOWN_NEEDLE),
        "stderr={restarted_stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_queue_drain_uses_consumer_manifest_and_persists_response_records() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "producer/harn.toml",
        r#"
[package]
name = "worker-producer"

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["a2a.task.received"] }
handler = "worker://triage"
priority = "high"
"#,
    );
    write_file(
        temp.path(),
        "consumer/harn.toml",
        r#"
[package]
name = "worker-consumer"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["a2a.task.received"] }
handler = "handlers::on_task"
"#,
    );
    write_file(
        temp.path(),
        "consumer/lib.harn",
        r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) -> dict {
  return {
    ok: true,
    kind: event.kind,
    event_id: event.id,
  }
}
"#,
    );

    let fire = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "fire",
            "incoming-review-task",
            "--config",
            "producer/harn.toml",
            "--state-dir",
            "./state",
        ],
        &[],
    );
    assert!(
        fire.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        fire.status.code(),
        stdout(&fire),
        stderr(&fire)
    );
    assert!(stdout(&fire).contains("\"queue\":\"triage\""));

    let queue_before = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "queue",
            "--config",
            "consumer/harn.toml",
            "--state-dir",
            "./state",
            "ls",
            "--json",
        ],
        &[],
    );
    assert!(
        queue_before.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        queue_before.status.code(),
        stdout(&queue_before),
        stderr(&queue_before)
    );
    let queue_before_json: serde_json::Value =
        serde_json::from_str(&stdout(&queue_before)).expect("queue ls JSON");
    assert_eq!(
        queue_before_json["worker_queues"][0]["queue"],
        serde_json::json!("triage")
    );
    assert_eq!(
        queue_before_json["worker_queues"][0]["ready"],
        serde_json::json!(1)
    );

    let drain = run_harn_with_env(
        &temp,
        &[
            "orchestrator",
            "queue",
            "--config",
            "consumer/harn.toml",
            "--state-dir",
            "./state",
            "drain",
            "triage",
            "--consumer-id",
            "consumer-a",
            "--json",
        ],
        &[],
    );
    assert!(
        drain.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        drain.status.code(),
        stdout(&drain),
        stderr(&drain)
    );
    let drain_json: serde_json::Value =
        serde_json::from_str(&stdout(&drain)).expect("queue drain JSON");
    assert_eq!(drain_json["drained"], serde_json::json!(1));
    assert_eq!(drain_json["acked"], serde_json::json!(1));
    assert_eq!(drain_json["deferred"], serde_json::json!(0));
    assert_eq!(
        drain_json["responses"][0]["outcome"]["status"],
        serde_json::json!("succeeded")
    );
    assert_eq!(drain_json["summary"]["ready"], serde_json::json!(0));
    assert_eq!(drain_json["summary"]["responses"], serde_json::json!(1));
}
