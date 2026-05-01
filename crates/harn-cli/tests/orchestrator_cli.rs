// Most tests in this file run the orchestrator in-process via
// [`OrchestratorHarness`]. A handful of subprocess assertions (process
// exit code semantics, raw stderr scraping) remain `#[ignore]`d
// pending the slow E2E/smoke lane in issue #1069.
//
// `harn_state_lock` returns a `std::sync::MutexGuard`, which is held
// across `.await` points in tests that read the on-disk event log
// after the harness shuts down. The lock only protects process-wide
// env vars from concurrent flips — it never crosses thread boundaries
// while held — so the `await_holding_lock` lint is intentionally
// silenced here.
#![allow(clippy::await_holding_lock)]

mod test_util;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use harn_cli::commands::orchestrator::harness::{OrchestratorConfig, OrchestratorHarness};
use harn_cli::env_guard::ScopedEnvVar;
use harn_cli::tests::common::{env_lock, harn_state_lock};
use harn_vm::event_log::{
    AnyEventLog, ConsumerId, EventLog, EventLogBackendKind, EventLogConfig, LogEvent, Topic,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tempfile::TempDir;
use test_util::process::harn_command;
use tokio::sync::MutexGuard;

const EVENT_FAIL_FAST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

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

// ── In-process harness helpers ───────────────────────────────────────────────

struct EnvGuards {
    _lock: MutexGuard<'static, ()>,
    _vars: Vec<ScopedEnvVar>,
}

async fn lock_env_with(envs: &[(&'static str, &str)]) -> EnvGuards {
    let lock = env_lock::lock_env().lock().await;
    let vars = envs
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(key, value))
        .collect();
    EnvGuards {
        _lock: lock,
        _vars: vars,
    }
}

fn test_config(temp: &TempDir) -> OrchestratorConfig {
    OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"))
}

async fn start_harness(temp: &TempDir) -> OrchestratorHarness {
    OrchestratorHarness::start(test_config(temp))
        .await
        .expect("orchestrator harness start")
}

async fn start_harness_with(
    temp: &TempDir,
    customize: impl FnOnce(OrchestratorConfig) -> OrchestratorConfig,
) -> OrchestratorHarness {
    OrchestratorHarness::start(customize(test_config(temp)))
        .await
        .expect("orchestrator harness start")
}

async fn shutdown(harness: OrchestratorHarness) {
    harness
        .shutdown(SHUTDOWN_DEADLINE)
        .await
        .expect("harness shutdown");
}

async fn await_topic_event(
    event_log: &Arc<AnyEventLog>,
    topic: &str,
    predicate: impl Fn(&LogEvent) -> bool,
) -> LogEvent {
    let topic_obj = Topic::new(topic).unwrap();
    let mut stream = event_log
        .clone()
        .subscribe(&topic_obj, None)
        .await
        .expect("subscribe");
    tokio::time::timeout(EVENT_FAIL_FAST_TIMEOUT, async {
        loop {
            let next = stream
                .next()
                .await
                .expect("stream ended")
                .expect("stream error");
            if predicate(&next.1) {
                return next.1;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for matching {topic} event"))
}

async fn await_topic_kind(event_log: &Arc<AnyEventLog>, topic: &str, kind: &str) -> LogEvent {
    let owned_kind = kind.to_string();
    await_topic_event(event_log, topic, move |event| event.kind == owned_kind).await
}

async fn await_topic_event_count(
    event_log: &Arc<AnyEventLog>,
    topic: &str,
    kind: &str,
    expected: usize,
) {
    if expected == 0 {
        return;
    }
    let topic_obj = Topic::new(topic).unwrap();
    let existing = event_log
        .read_range(&topic_obj, None, usize::MAX)
        .await
        .expect("read_range")
        .into_iter()
        .filter(|(_, event)| event.kind == kind)
        .count();
    if existing >= expected {
        return;
    }
    let mut stream = event_log
        .clone()
        .subscribe(&topic_obj, None)
        .await
        .expect("subscribe");
    let mut total = existing;
    let owned_kind = kind.to_string();
    tokio::time::timeout(EVENT_FAIL_FAST_TIMEOUT, async {
        while total < expected {
            let (_, event) = stream
                .next()
                .await
                .expect("stream ended")
                .expect("stream error");
            if event.kind == owned_kind {
                total += 1;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {topic}/{kind} count {expected}"));
}

async fn read_topic_events(event_log: &Arc<AnyEventLog>, topic: &str) -> Vec<(u64, LogEvent)> {
    event_log
        .read_range(&Topic::new(topic).unwrap(), None, usize::MAX)
        .await
        .expect("read_range")
}

async fn wait_for_consumer_cursor(
    event_log: &Arc<AnyEventLog>,
    topic_name: &str,
    consumer: &str,
    at_least: u64,
) {
    let topic = Topic::new(topic_name).unwrap();
    let consumer = ConsumerId::new(consumer).unwrap();
    let deadline = tokio::time::Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let cursor = event_log
            .consumer_cursor(&topic, &consumer)
            .await
            .unwrap()
            .unwrap_or(0);
        if cursor >= at_least {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for consumer cursor {consumer} on {topic_name} to reach {at_least}");
}

fn open_state_event_log(state_dir: &Path) -> Arc<AnyEventLog> {
    let mut config = EventLogConfig::for_base_dir(state_dir).unwrap();
    let file_dir = state_dir.join("events");
    if file_dir.join("topics").is_dir() {
        config.backend = EventLogBackendKind::File;
        config.file_dir = file_dir;
    }
    harn_vm::event_log::open_event_log(&config).unwrap()
}

async fn read_topic_events_from_state(state_dir: &Path, topic_name: &str) -> Vec<(u64, LogEvent)> {
    let log = open_state_event_log(state_dir);
    log.read_range(&Topic::new(topic_name).unwrap(), None, usize::MAX)
        .await
        .unwrap()
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
    let deadline = tokio::time::Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    let mut last = String::new();
    while tokio::time::Instant::now() < deadline {
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for metrics samples {needles:?}; last={last}");
}

fn run_harn_with_env(temp: &TempDir, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = harn_command();
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

fn seed_legacy_inbox_records(temp: &TempDir) {
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let config = EventLogConfig::for_base_dir(&state_dir).unwrap();
    let log = harn_vm::event_log::open_event_log(&config).unwrap();

    let legacy_topic = Topic::new(harn_vm::TRIGGER_INBOX_LEGACY_TOPIC).unwrap();
    // Use `i64::MAX` so the dedupe claim is treated as still-active for the
    // duration of the test regardless of wall-clock skew on shared CI.
    // `InboxIndex` checks `claim.expires_at_ms > now_ms` (see
    // `crates/harn-vm/src/triggers/inbox.rs`); a never-expiring fixture lets
    // us assert the legacy record is honored without scheduling a real
    // expiry deadline against the system clock.
    let future_expiry_ms = i64::MAX;
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[ignore = "asserts on raw orchestrator stderr — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn orchestrator_serve_starts_and_shuts_down_cleanly() {}

// Regression coverage for harn#325: graceful shutdown should let an in-flight
// a2a-push dispatch finish within the configured shutdown window and emit the
// terminal `dispatch_succeeded` lifecycle event instead of a shutdown failure.
#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_drains_in_flight_dispatch_and_emits_lifecycle_events() {
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

    let _envs = lock_env_with(&[
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();
    let event_log = harness.event_log();
    let shutdown_trigger = harness.shutdown_trigger();

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-123"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    await_topic_kind(&event_log, "triggers.lifecycle", "DispatchStarted").await;
    let _ = shutdown_trigger.send(true);
    fs::write(&handler_release_path, b"release").unwrap();
    shutdown(harness).await;

    let lifecycle = read_topic_events(&event_log, "orchestrator.lifecycle").await;
    assert!(lifecycle.iter().any(|(_, event)| event.kind == "draining"));
    assert!(lifecycle.iter().any(|(_, event)| {
        event.kind == "stopped" && event.payload["timed_out"] == serde_json::json!(false)
    }));

    let inbox = read_topic_events(&event_log, harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC).await;
    assert!(
        inbox
            .iter()
            .any(|(_, event)| event.kind == "event_ingested"),
        "inbox={inbox:?}"
    );
    let legacy_inbox = read_topic_events(&event_log, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC).await;
    assert!(legacy_inbox.is_empty(), "legacy_inbox={legacy_inbox:?}");

    let outbox = read_topic_events(&event_log, "trigger.outbox").await;
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

    let _envs = lock_env_with(&[
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_TEST_ORCHESTRATOR_FAIL_PENDING_PUMP", "1"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();
    let event_log = harness.event_log();

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-240"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    shutdown(harness).await;

    let lifecycle = read_topic_events(&event_log, "orchestrator.lifecycle").await;
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

    let _envs = lock_env_with(&[
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            inbox_release_value.as_str(),
        ),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();
    let event_log = harness.event_log();
    let shutdown_trigger = harness.shutdown_trigger();

    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-241"}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    await_topic_event(&event_log, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    await_topic_event(&event_log, "orchestrator.lifecycle", |event| {
        event.kind == "pump_acked" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;

    let _ = shutdown_trigger.send(true);
    fs::write(&inbox_release_file, b"release").unwrap();
    shutdown(harness).await;

    let outbox = read_topic_events(&event_log, "trigger.outbox").await;
    assert!(
        outbox
            .iter()
            .any(|(_, event)| event.kind == "dispatch_succeeded"),
        "outbox={outbox:?}"
    );
    let lifecycle = read_topic_events(&event_log, "orchestrator.lifecycle").await;
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

    let _envs = lock_env_with(&[
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        (
            "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE",
            inbox_release_value.as_str(),
        ),
    ])
    .await;
    let harness = start_harness_with(&temp, |mut config| {
        config.pump.max_outstanding = 1;
        config
    })
    .await;
    let base_url = harness.listener_url().to_string();
    let event_log = harness.event_log();
    let shutdown_trigger = harness.shutdown_trigger();
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
        &event_log,
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        &format!(
            "orchestrator-pump.{}",
            harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC
        ),
        1,
    )
    .await;
    await_topic_event(&event_log, "orchestrator.lifecycle", |event| {
        event.kind == "pump_admitted" && event.payload["event_log_id"] == serde_json::json!(1)
    })
    .await;
    await_topic_event_count(
        &event_log,
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        "event_ingested",
        2,
    )
    .await;

    let topic = Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap();
    let consumer = ConsumerId::new(format!(
        "orchestrator-pump.{}",
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC
    ))
    .unwrap();
    let cursor = event_log.consumer_cursor(&topic, &consumer).await.unwrap();
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

    let _ = shutdown_trigger.send(true);
    fs::write(&inbox_release_file, b"release").unwrap();
    shutdown(harness).await;

    let outbox = read_topic_events(&event_log, "trigger.outbox").await;
    assert_eq!(
        outbox
            .iter()
            .filter(|(_, event)| event.kind == "dispatch_succeeded")
            .count(),
        2,
        "outbox={outbox:?}"
    );
    let lifecycle = read_topic_events(&event_log, "orchestrator.lifecycle").await;
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

#[tokio::test(flavor = "multi_thread")]
async fn orchestrator_queue_soft_migrates_legacy_inbox_topics() {
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
    // Acquire env_lock + harn_state_lock so concurrent tests can't
    // flip `HARN_EVENT_LOG_BACKEND` or leak `HARN_STATE_DIR`
    // mid-seed. `OrchestratorRole::build_vm` will also set
    // `HARN_STATE_DIR` once the harness starts; pin it to this
    // test's `state_dir` up-front so seed reads and harness reads
    // resolve to the same SQLite path.
    let _envs = lock_env_with(&[]).await;
    let _state_lock = harn_state_lock::lock_harn_state();
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let _state_dir_var = ScopedEnvVar::set(
        harn_vm::runtime_paths::HARN_STATE_DIR_ENV,
        state_dir.to_str().unwrap(),
    );
    seed_legacy_inbox_records(&temp);
    let legacy_before =
        read_topic_events_from_state(&state_dir, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC).await;
    assert_eq!(legacy_before.len(), 2, "legacy_before={legacy_before:?}");

    let harness = start_harness(&temp).await;
    shutdown(harness).await;

    let legacy_after =
        read_topic_events_from_state(&state_dir, harn_vm::TRIGGER_INBOX_LEGACY_TOPIC).await;
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
    let inbox = harn_vm::InboxIndex::new(log.clone(), metrics)
        .await
        .unwrap();
    assert!(!inbox
        .insert_if_new("github-new-issue", "delivery-123", Duration::from_secs(60))
        .await
        .unwrap());
}

// Regression coverage for harn#328: a bounded drain should truncate backlog on
// shutdown, persist each pump's consumer cursor in the event log, and let the
// next orchestrator run replay the remaining backlog to completion.
#[tokio::test(flavor = "multi_thread")]
async fn bounded_pump_drain_truncates_and_replays_remaining_backlog_after_restart() {
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

    let _envs = lock_env_with(&[
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
    ])
    .await;
    let harness = start_harness_with(&temp, |mut config| {
        config.drain.max_items = 10;
        config.drain.deadline = Duration::from_secs(1);
        config
    })
    .await;
    let base_url = harness.listener_url().to_string();
    let shutdown_trigger = harness.shutdown_trigger();

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

    wait_for_path_async(&pump_waiting_file).await;
    let _ = shutdown_trigger.send(true);
    wait_for_path_async(&pump_draining_file).await;
    fs::write(&pump_release_file, b"release").unwrap();
    shutdown(harness).await;

    drop(_envs);

    let _envs = lock_env_with(&[
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_EVENT_LOG_QUEUE_DEPTH", "8192"),
    ])
    .await;
    let harness = start_harness_with(&temp, |mut config| {
        config.drain.max_items = 10;
        config.drain.deadline = Duration::from_secs(1);
        config
    })
    .await;
    let event_log = harness.event_log();
    await_topic_event_count(
        &event_log,
        "trigger.outbox",
        "dispatch_succeeded",
        TOTAL_EVENTS,
    )
    .await;
    shutdown(harness).await;

    drop(_envs);

    let final_outbox = {
        let state_dir = temp.path().join("state");
        read_topic_events_from_state(&state_dir, "trigger.outbox").await
    };
    assert_eq!(
        final_outbox
            .iter()
            .filter(|(_, event)| event.kind == "dispatch_succeeded")
            .count(),
        TOTAL_EVENTS,
        "outbox count mismatch"
    );
}

#[ignore = "uses std::process::exit(86) for crash simulation — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn restart_surfaces_stranded_envelopes_and_recover_replays_them_explicitly() {}

#[ignore = "asserts on subprocess stdout for `harn orchestrator fire`/`queue ls`/`queue drain` — moves to slow E2E/smoke job (issue #1069)"]
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

async fn wait_for_path_async(path: &Path) {
    let deadline = tokio::time::Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for path {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
