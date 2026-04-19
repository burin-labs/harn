#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::event_log::{EventLog, EventLogBackendKind, EventLogConfig, LogEvent, Topic};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tempfile::TempDir;

const STARTUP_NEEDLE: &str = "HTTP listener ready on";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn spawn_orchestrator(temp: &TempDir) -> (Child, Receiver<String>, thread::JoinHandle<String>) {
    spawn_orchestrator_with(temp, &[], &[])
}

fn spawn_orchestrator_with(
    temp: &TempDir,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> (Child, Receiver<String>, thread::JoinHandle<String>) {
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

    (child, rx, handle)
}

fn wait_for_log_line(child: &mut Child, rx: &Receiver<String>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains(needle) => return,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().unwrap() {
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

fn wait_for_listener_url(child: &mut Child, rx: &Receiver<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains(STARTUP_NEEDLE) => {
                return line
                    .split(STARTUP_NEEDLE)
                    .nth(1)
                    .expect("startup URL suffix")
                    .trim()
                    .to_string();
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("process exited before listener became ready: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("stderr stream closed before listener became ready");
            }
        }
    }
    panic!("timed out waiting for listener URL");
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
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "child exited unsuccessfully: {status}");
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
}

fn wait_for_any_exit(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
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
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if read_topic_events(state_dir, topic_name)
            .iter()
            .any(|(_, event)| event.kind == kind)
        {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {topic_name}/{kind}");
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
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if sqlite_event_count(state_dir, topic_name, kind) >= expected {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {topic_name}/{kind} count {expected}");
}

#[test]
fn orchestrator_serve_starts_and_shuts_down_cleanly() {
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
    send_sigterm(&child);
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(stderr.contains("secret providers:"), "stderr={stderr}");
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

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_drains_in_flight_dispatch_and_emits_lifecycle_events() {
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
  sleep(1000)
  return event.kind
}
"#,
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
    send_sigterm(&child);
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

#[test]
fn orchestrator_queue_soft_migrates_legacy_inbox_topics() {
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
    child.kill().unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn bounded_pump_drain_truncates_and_replays_remaining_backlog_after_restart() {
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
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_EVENT_LOG_QUEUE_DEPTH", "8192"),
        ("HARN_TEST_ORCHESTRATOR_PUMP_DELAY_MS", "50"),
    ];
    let extra_args = [
        "--shutdown-timeout",
        "5",
        "--drain-max-items",
        "100",
        "--drain-deadline",
        "1",
    ];
    let (mut child, rx, handle) = spawn_orchestrator_with(&temp, &extra_args, &envs);
    let base_url = wait_for_listener_url(&mut child, &rx);
    let state_dir = temp.path().join("state");

    let client = reqwest::Client::new();
    for index in 0..5000 {
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

    let shutdown_started = Instant::now();
    send_sigterm(&child);
    wait_for_exit(&mut child);
    let shutdown_elapsed = shutdown_started.elapsed();
    let stderr = handle.join().expect("stderr collector thread");

    assert!(
        shutdown_elapsed < Duration::from_secs(4),
        "shutdown should finish within bounded drain budget: {shutdown_elapsed:?}"
    );
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
    wait_for_sqlite_event_count(&state_dir, "trigger.outbox", "dispatch_succeeded", 5000);
    send_sigterm(&restart_child);
    wait_for_exit(&mut restart_child);
    let restart_stderr = restart_handle.join().expect("stderr collector thread");
    assert!(
        restart_stderr.contains(SHUTDOWN_NEEDLE),
        "stderr={restart_stderr}"
    );

    assert_eq!(
        sqlite_event_count(&state_dir, "trigger.outbox", "dispatch_succeeded"),
        5000
    );
}
