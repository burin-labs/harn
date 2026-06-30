use super::*;
use futures::StreamExt;
use harn_vm::event_log::{EventLog, Topic};

fn write_test_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn stream_manifest_fixture() -> &'static str {
    r#"
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
}

fn stream_handler_fixture(marker_path: &Path) -> String {
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

/// Proof test: `stream_trigger_route_uses_generic_stream_connector`
/// migrated from subprocess + SQLite polling to in-process harness +
/// `EventLog::subscribe()`.
#[tokio::test(flavor = "multi_thread")]
async fn stream_trigger_route_uses_generic_stream_connector_in_process() {
    // Env vars are process-global; hold the lock for the entire test so
    // concurrent unit tests that also set env vars don't race.
    let _env_lock = crate::tests::common::env_lock::lock_env().lock().await;
    let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");

    let temp = tempfile::TempDir::new().unwrap();
    let marker_path = temp.path().join("stream-handler.json");
    write_test_file(temp.path(), "harn.toml", stream_manifest_fixture());
    write_test_file(
        temp.path(),
        "lib.harn",
        &stream_handler_fixture(&marker_path),
    );

    let config =
        OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"));
    let harness = OrchestratorHarness::start(config)
        .await
        .expect("harness start");
    let base_url = harness.listener_url().to_string();
    let event_log = harness.event_log();

    let response = reqwest::Client::new()
        .post(format!("{base_url}/streams/ws"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "key": "acct-1",
            "stream": "quotes",
            "value": {"amount": 10}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Event-driven wait: subscribe to orchestrator.lifecycle and block
    // until pump_dispatch_completed arrives, replacing SQLite polling.
    let topic = Topic::new("orchestrator.lifecycle").unwrap();
    let mut stream = event_log.clone().subscribe(&topic, None).await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for pump_dispatch_completed");
        let (_, event) = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for pump_dispatch_completed event")
            .expect("event stream ended unexpectedly")
            .expect("event stream error");
        if event.kind == "pump_dispatch_completed"
            && event.payload["status"] == serde_json::json!("completed")
        {
            break;
        }
    }
    drop(stream);

    let marker: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
    assert_eq!(
        marker.get("provider").and_then(|v| v.as_str()),
        Some("websocket")
    );
    assert_eq!(
        marker.get("kind").and_then(|v| v.as_str()),
        Some("quote.tick")
    );
    assert_eq!(marker.get("key").and_then(|v| v.as_str()), Some("acct-1"));
    assert_eq!(
        marker.get("stream").and_then(|v| v.as_str()),
        Some("quotes")
    );
    assert_eq!(marker.get("amount").and_then(|v| v.as_i64()), Some(10));

    harness
        .shutdown(std::time::Duration::from_secs(5))
        .await
        .expect("harness shutdown");
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_start_waits_for_topic_pumps_before_ready() {
    let _env_lock = crate::tests::common::env_lock::lock_env().lock().await;
    let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");

    let temp = tempfile::TempDir::new().unwrap();
    let marker_path = temp.path().join("stream-handler.json");
    write_test_file(temp.path(), "harn.toml", stream_manifest_fixture());
    write_test_file(
        temp.path(),
        "lib.harn",
        &stream_handler_fixture(&marker_path),
    );

    let config =
        OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"));
    let harness = OrchestratorHarness::start(config)
        .await
        .expect("harness start");
    let event_log = harness.event_log();
    let topic = Topic::new("orchestrator.lifecycle").unwrap();
    let lifecycle = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .expect("read lifecycle");

    let (pumps_ready_id, pumps_ready) = lifecycle
        .iter()
        .find(|(_, event)| event.kind == "pumps_ready")
        .expect("pumps_ready lifecycle event");
    let (startup_id, _) = lifecycle
        .iter()
        .find(|(_, event)| event.kind == "startup")
        .expect("startup lifecycle event");
    assert!(
        pumps_ready_id < startup_id,
        "pumps_ready must precede startup: lifecycle={lifecycle:?}"
    );

    let topics = pumps_ready
        .payload
        .get("topics")
        .and_then(serde_json::Value::as_array)
        .expect("pumps_ready topics");
    for expected in [
        "orchestrator.triggers.pending",
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        "connectors.cron.tick",
        harn_vm::WAITPOINT_RESUME_TOPIC,
        harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC,
    ] {
        assert!(
            topics.iter().any(|topic| topic.as_str() == Some(expected)),
            "missing pump topic {expected}: topics={topics:?}"
        );
    }

    harness
        .shutdown(std::time::Duration::from_secs(5))
        .await
        .expect("harness shutdown");
}
