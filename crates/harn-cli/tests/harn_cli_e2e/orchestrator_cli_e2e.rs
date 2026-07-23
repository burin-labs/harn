//! Real-process contracts for `harn orchestrator`.
//!
//! The fast orchestrator suites exercise the typed in-process harness. This
//! target owns behavior that only a real CLI process can prove: readiness and
//! shutdown logs, POSIX signal handling, crash exit codes, and recovery CLI
//! projections over state persisted by a previous process.

#[path = "orchestrator_cli_e2e/support.rs"]
mod support;
use crate::test_util;

use std::net::SocketAddr;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use tempfile::TempDir;

use support::{
    run_orchestrator_command, stderr, stdout, wait_for_topic_event, write_file,
    OrchestratorProcess, SHUTDOWN_NEEDLE,
};
use test_util::connectors::{provider_declarations, write_first_party_connector_modules};

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SnapshotStatus {
    Stopped,
}

#[derive(Debug, Deserialize)]
struct OrchestratorStateSnapshot {
    status: SnapshotStatus,
    bind: SocketAddr,
}

fn bearer_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key"));
    headers
}

fn assert_success(label: &str, output: &std::process::Output) -> String {
    let stdout = stdout(output);
    let stderr = stderr(output);
    assert!(
        output.status.success(),
        "{label} failed: status={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status.code()
    );
    stdout
}

#[tokio::test]
async fn orchestrator_serve_starts_and_shuts_down_cleanly() {
    let temp = TempDir::new().unwrap();
    write_first_party_connector_modules(temp.path());
    write_file(
        temp.path(),
        "harn.toml",
        &format!(
            r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

{}

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = {{ events = ["issues.opened"] }}
handler = "handlers::on_issue"
secrets = {{ signing_secret = "github/webhook-secret" }}
"#,
            provider_declarations()
        ),
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

    let mut process = OrchestratorProcess::spawn(&temp, &[]);
    let listener = process.wait_for_listener_url().await;
    let stderr = process.terminate_gracefully().await;

    assert!(stderr.contains("secret providers:"), "stderr={stderr}");
    assert!(
        stderr.contains("registered triggers (1):"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("registered connectors (1): github"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot: OrchestratorStateSnapshot = serde_json::from_slice(
        &std::fs::read(temp.path().join("state/orchestrator-state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot.status, SnapshotStatus::Stopped);
    assert!(snapshot.bind.ip().is_loopback());
    assert_ne!(snapshot.bind.port(), 0);
    assert_eq!(listener, format!("http://{}", snapshot.bind));
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_surfaces_stranded_envelopes_and_recover_replays_them_explicitly() {
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

    let crash_env = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
        ("HARN_TEST_DISPATCHER_FAIL_BEFORE_OUTBOX", "1"),
    ];
    let mut crashing = OrchestratorProcess::spawn(&temp, &crash_env);
    let base_url = crashing.wait_for_listener_url().await;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers())
        .body(r#"{"kind":"a2a.task.received","task":{"id":"task-242"}}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let crashing_stderr = crashing.wait_for_exit_code(86).await;
    assert!(
        crashing_stderr.contains("registered connectors (1): a2a-push"),
        "stderr={crashing_stderr}"
    );

    let restart_env = [
        ("HARN_EVENT_LOG_BACKEND", "file"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "unused-shared-secret"),
    ];
    let mut restarted = OrchestratorProcess::spawn(&temp, &restart_env);
    restarted.wait_for_listener_url().await;
    let state_dir = temp.path().join("state");
    let stranded = wait_for_topic_event(&state_dir, "orchestrator.lifecycle", |event| {
        event.kind == "startup_stranded_envelopes" && event.payload["count"] == 1
    })
    .await;
    assert_eq!(stranded.payload["count"], 1);

    let queue =
        run_orchestrator_command(&temp, "queue", &[], &[("HARN_EVENT_LOG_BACKEND", "file")]).await;
    assert!(assert_success("queue", &queue).contains("stranded_envelopes=1"));

    let recover_without_yes = run_orchestrator_command(
        &temp,
        "recover",
        &["--envelope-age", "0s"],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    )
    .await;
    assert!(!recover_without_yes.status.success());
    assert!(stderr(&recover_without_yes).contains("without --yes"));

    let dry_run = run_orchestrator_command(
        &temp,
        "recover",
        &["--envelope-age", "0s", "--dry-run"],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    )
    .await;
    let dry_run_stdout = assert_success("recover dry run", &dry_run);
    assert!(dry_run_stdout.contains("stranded_envelopes=1"));
    assert!(dry_run_stdout.contains("event_id=trigger_evt_"));

    let recover = run_orchestrator_command(
        &temp,
        "recover",
        &["--envelope-age", "0s", "--yes"],
        &[("HARN_EVENT_LOG_BACKEND", "file")],
    )
    .await;
    assert!(assert_success("recover", &recover).contains("status=dispatched"));

    let queue_after =
        run_orchestrator_command(&temp, "queue", &[], &[("HARN_EVENT_LOG_BACKEND", "file")]).await;
    assert!(assert_success("queue after recovery", &queue_after).contains("stranded_envelopes=0"));

    let restarted_stderr = restarted.terminate_gracefully().await;
    assert!(
        restarted_stderr.contains(SHUTDOWN_NEEDLE),
        "stderr={restarted_stderr}"
    );
}
