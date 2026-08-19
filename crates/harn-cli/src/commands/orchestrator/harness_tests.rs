use super::*;
use futures::StreamExt;
use harn_vm::event_log::{AnyEventLog, EventLog, Topic};

fn write_test_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

async fn wait_for_persona_completion(event_log: &std::sync::Arc<AnyEventLog>) {
    let topic = Topic::new(harn_vm::personas::PERSONA_RUNTIME_TOPIC).unwrap();
    let mut stream = event_log.clone().subscribe(&topic, None).await.unwrap();
    let existing = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .expect("read persona lifecycle");
    if let Some((_, failed)) = existing
        .iter()
        .find(|(_, event)| event.kind == "persona.run.failed")
    {
        panic!("persona run failed: {}", failed.payload);
    }
    if !existing
        .iter()
        .any(|(_, event)| event.kind == "persona.run.completed")
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .expect("timed out waiting for persona completion");
            let (_, event) = tokio::time::timeout(remaining, stream.next())
                .await
                .expect("timed out waiting for persona completion event")
                .expect("persona event stream ended unexpectedly")
                .expect("persona event stream error");
            match event.kind.as_str() {
                "persona.run.completed" => break,
                "persona.run.failed" => panic!("persona run failed: {}", event.payload),
                _ => {}
            }
        }
    }
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

/// Proof test: `stream_trigger_route_uses_generic_stream_connector`
/// migrated from subprocess + SQLite polling to in-process harness +
/// `EventLog::subscribe()`.
#[tokio::test(flavor = "multi_thread")]
async fn stream_trigger_route_uses_generic_stream_connector_in_process() {
    // Env vars are process-global; hold the lock for the entire test so
    // concurrent unit tests that also set env vars don't race.
    let _env_lock = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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
async fn cron_persona_route_executes_entry_workflow_before_completion() {
    let _env_lock = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");
    let clock = harn_vm::clock::PausedClock::new(
        time::OffsetDateTime::from_unix_timestamp(1_784_195_970).unwrap(),
    );

    let temp = tempfile::TempDir::new().unwrap();
    let marker_path = temp.path().join("persona-ran.json");
    write_test_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "persona-cron-fixture"

[[personas]]
name = "digest"
description = "Runs the digest workflow."
entry_workflow = "digest.harn#run"
tools = ["filesystem"]
capabilities = ["workspace.write_text"]
autonomy = "act_auto"
receipts = "required"

[[triggers]]
id = "digest-cron"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "persona://digest"
schedule = "0 * * * *"
timezone = "UTC"
"#,
    );
    write_test_file(
        temp.path(),
        "digest.harn",
        &format!(
            r"
pub pipeline run(harness: Harness, event: TriggerEvent) {{
  let result = {{executed: true, provider: event.provider, kind: event.kind}}
  harness.fs.write_text({marker:?}, json_stringify(result))
  return result
}}
",
            marker = marker_path.display().to_string()
        ),
    );

    let config =
        OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"))
            .with_clock(clock.clone());
    let harness = OrchestratorHarness::start(config)
        .await
        .expect("harness start");
    let event_log = harness.event_log();
    let topic = Topic::new(harn_vm::personas::PERSONA_RUNTIME_TOPIC).unwrap();
    clock.advance(std::time::Duration::from_secs(30));
    wait_for_persona_completion(&event_log).await;

    let marker: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
    assert_eq!(marker["executed"], true);
    assert_eq!(marker["provider"], "cron");
    assert_eq!(marker["kind"], "tick");

    let lifecycle = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .expect("read completed persona lifecycle");
    let kinds = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect::<Vec<_>>();
    let started = kinds
        .iter()
        .position(|kind| *kind == "persona.run.started")
        .expect("persona run started");
    let completed = kinds
        .iter()
        .position(|kind| *kind == "persona.run.completed")
        .expect("persona run completed");
    assert!(started < completed);
    assert!(!kinds.contains(&"persona.run.failed"));

    harness
        .shutdown(std::time::Duration::from_secs(5))
        .await
        .expect("harness shutdown");
}

#[test]
fn materialized_cron_persona_executes_its_generated_entry_workflow() {
    crate::tests::common::async_runtime::block_on_cli_stack_multi_thread(|| async {
        let _env_lock = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
        let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");
        let clock = harn_vm::clock::PausedClock::new(
            time::OffsetDateTime::from_unix_timestamp(1_784_195_970).unwrap(),
        );
        let temp = tempfile::TempDir::new().unwrap();
        let blueprint = temp.path().join("blueprint.json");
        std::fs::write(
            &blueprint,
            serde_json::json!({
                "schema_version": "1",
                "name": "reply_digest",
                "description": "Summarizes follow-up work.",
                "goal": "Surface replies every hour.",
                "template": "deterministic-sweeper",
                "cron": {"cron": "0 * * * *", "timezone": "UTC"},
            })
            .to_string(),
        )
        .unwrap();
        let package = crate::commands::persona_scaffold::materialize_persona_package(
            &blueprint,
            &temp.path().join("personas"),
            false,
        )
        .await
        .expect("materialize cron persona");

        let config =
            OrchestratorConfig::for_test(package.root.join("harn.toml"), temp.path().join("state"))
                .with_clock(clock.clone());
        let harness = OrchestratorHarness::start(config)
            .await
            .expect("harness start");
        let event_log = harness.event_log();
        let topic = Topic::new(harn_vm::personas::PERSONA_RUNTIME_TOPIC).unwrap();
        clock.advance(std::time::Duration::from_secs(30));
        wait_for_persona_completion(&event_log).await;

        let lifecycle = event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .expect("read materialized persona lifecycle");
        let completed = lifecycle
            .iter()
            .find(|(_, event)| event.kind == "persona.run.completed")
            .expect("materialized persona completed");
        assert_eq!(
            completed.1.payload["entry_workflow"].as_str(),
            Some("src/reply_digest.harn#run")
        );
        assert!(!lifecycle
            .iter()
            .any(|(_, event)| event.kind == "persona.run.failed"));

        harness
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .expect("harness shutdown");
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_start_waits_for_topic_pumps_before_ready() {
    let _env_lock = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
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

/// Boot the orchestrator in each role and read back what it actually told the
/// operator about registry isolation.
///
/// The role's own unit tests assert the strings in isolation, which is not the
/// same claim: they would still pass if `orchestrator_lifecycle` never consulted
/// `unproven_isolation` at all. This one drives the real start path and asserts
/// the disclosure reached the lifecycle log, so deleting the emission fails a
/// test instead of quietly restoring the silence the role used to boot with.
///
/// Multi-tenant shares one trigger/connector registry across every tenant
/// because `build_vm` makes one VM per process; single-tenant delivers the
/// boundary its name implies and must stay quiet, or the warning becomes noise
/// operators learn to skip.
#[tokio::test(flavor = "multi_thread")]
async fn multi_tenant_boot_records_the_shared_registry_gap_and_single_tenant_does_not() {
    async fn isolation_gap_payload_after_boot(
        role: crate::commands::orchestrator::role::OrchestratorRole,
    ) -> Option<serde_json::Value> {
        let temp = tempfile::TempDir::new().unwrap();
        let marker_path = temp.path().join("stream-handler.json");
        write_test_file(temp.path(), "harn.toml", stream_manifest_fixture());
        write_test_file(
            temp.path(),
            "lib.harn",
            &stream_handler_fixture(&marker_path),
        );

        let mut config =
            OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"));
        config.role = role;

        let harness = OrchestratorHarness::start(config)
            .await
            .expect("harness start");
        let topic = Topic::new("orchestrator.lifecycle").unwrap();
        let lifecycle = harness
            .event_log()
            .read_range(&topic, None, usize::MAX)
            .await
            .expect("read lifecycle");
        let gap = lifecycle
            .iter()
            .find(|(_, event)| event.kind == "isolation_gap")
            .map(|(_, event)| event.payload.clone());

        harness
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .expect("harness shutdown");
        gap
    }

    let _env_lock = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");

    let multi_tenant = isolation_gap_payload_after_boot(
        crate::commands::orchestrator::role::OrchestratorRole::MultiTenant,
    )
    .await
    .expect("multi-tenant boot must record the isolation gap it ships with");
    let gap = multi_tenant
        .get("gap")
        .and_then(serde_json::Value::as_str)
        .expect("isolation_gap payload carries the gap text");
    assert!(
        gap.contains("shares one registry"),
        "recorded gap must name the shared registry: {multi_tenant}"
    );
    assert!(
        gap.contains("6792"),
        "recorded gap must point at the tracking issue: {multi_tenant}"
    );
    assert_eq!(
        multi_tenant
            .get("registry_mode")
            .and_then(serde_json::Value::as_str),
        Some("one shared trigger/connector registry"),
        "recorded registry mode must match what build_vm constructs: {multi_tenant}"
    );

    assert_eq!(
        isolation_gap_payload_after_boot(
            crate::commands::orchestrator::role::OrchestratorRole::SingleTenant,
        )
        .await,
        None,
        "single-tenant delivers its boundary and must not warn"
    );
}
