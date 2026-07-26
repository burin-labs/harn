//! Regression coverage for the trigger stdlib surface.

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;

use time::OffsetDateTime;

use crate::event_log::{
    install_default_for_base_dir, pin_test_occurred_at_ms, EventLog, LogEvent, Topic,
};
use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
use crate::triggers::event::{CronEventPayload, KnownProviderPayload};
use crate::triggers::{
    TriggerBindingSource, TriggerBindingSpec, TriggerEvent, TriggerEventId, TriggerHandlerSpec,
    TriggerRegistryError, TriggerRetryConfig,
};
use crate::value::VmValue;
use crate::vm::Vm;
use crate::{install_manifest_triggers, register_vm_stdlib, ProviderId, ProviderPayload};

use super::journal::{DispatchHandleRecord, TriggerEventRecord};
use super::TRIGGER_EVENTS_TOPIC;
/// Build the `OffsetDateTime` for a pinned event-log `occurred_at_ms`, so
/// `received_at` cutoffs share the reference frame of
/// `pin_test_occurred_at_ms` without reading the wall clock.
fn offset_from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000)
        .expect("epoch ms within OffsetDateTime range")
}

fn manifest_binding(
    id: &str,
    fingerprint: &str,
    handler_name: &str,
    closure: std::sync::Arc<crate::value::VmClosure>,
) -> TriggerBindingSpec {
    TriggerBindingSpec {
        id: id.to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "cron".to_string(),
        provider: ProviderId::from("cron"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: handler_name.to_string(),
            callable: crate::value::VmCallable::Eager(closure),
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: TriggerRetryConfig::default(),
        match_events: vec!["cron.tick".to_string()],
        dedupe_key: None,
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: fingerprint.to_string(),
    }
}

fn recorded_cron_event(event_id: &str, received_at: OffsetDateTime) -> TriggerEvent {
    TriggerEvent {
        id: TriggerEventId(event_id.to_string()),
        provider: ProviderId::from("cron"),
        kind: "cron.tick".to_string(),
        received_at,
        occurred_at: None,
        dedupe_key: format!("delivery-{event_id}"),
        trace_id: crate::TraceId(format!("trace-{event_id}")),
        tenant_id: None,
        headers: BTreeMap::new(),
        batch: None,
        raw_body: None,
        provider_payload: ProviderPayload::Known(KnownProviderPayload::Cron(CronEventPayload {
            cron_id: Some("test-cron".to_string()),
            schedule: Some("* * * * *".to_string()),
            tick_at: received_at,
            raw: serde_json::json!({ "event_id": event_id }),
        })),
        signature_status: crate::SignatureStatus::Verified,
        dedupe_claimed: false,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_replay_falls_back_after_binding_version_gc() {
    crate::reset_thread_local_state();
    let sink = Rc::new(CollectorSink::new());
    clear_event_sinks();
    add_event_sink(sink.clone());

    let tempdir = tempfile::tempdir().expect("tempdir");
    let event_log = install_default_for_base_dir(tempdir.path()).expect("install event log");
    let lib_path = tempdir.path().join("lib.harn");
    fs::write(
        &lib_path,
        r#"
import "std/triggers"

pub fn on_tick_v1(event: TriggerEvent) -> dict {
  return {version: "v1", kind: event.kind}
}

pub fn on_tick_v2(event: TriggerEvent) -> dict {
  return {version: "v2", kind: event.kind}
}

pub fn on_tick_v3(event: TriggerEvent) -> dict {
  return {version: "v3", kind: event.kind}
}

pub fn on_tick_v4(event: TriggerEvent) -> dict {
  return {version: "v4", kind: event.kind}
}
"#,
    )
    .expect("write lib");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_project_root(tempdir.path());
    vm.set_source_dir(tempdir.path());
    let exports = vm
        .load_module_exports(&lib_path)
        .await
        .expect("load handler exports");

    // Pin the event-log timestamps: v1..v3 land at T1, v4 at T2 > T1, with
    // `received_at` at T1 so the gc fallback resolves the stale event to v3
    // (latest binding active at-or-before `received_at`) while v4 is later.
    const T1_MS: i64 = 1_700_000_000_000;
    const T2_MS: i64 = T1_MS + 10;
    let received_at = offset_from_ms(T1_MS);

    let clock = pin_test_occurred_at_ms(T1_MS);
    install_manifest_triggers(vec![manifest_binding(
        "replay-cron",
        "v1",
        "on_tick_v1",
        exports["on_tick_v1"].clone(),
    )])
    .await
    .expect("install v1");
    install_manifest_triggers(vec![manifest_binding(
        "replay-cron",
        "v2",
        "on_tick_v2",
        exports["on_tick_v2"].clone(),
    )])
    .await
    .expect("install v2");
    install_manifest_triggers(vec![manifest_binding(
        "replay-cron",
        "v3",
        "on_tick_v3",
        exports["on_tick_v3"].clone(),
    )])
    .await
    .expect("install v3");
    drop(clock);

    let clock = pin_test_occurred_at_ms(T2_MS);
    install_manifest_triggers(vec![manifest_binding(
        "replay-cron",
        "v4",
        "on_tick_v4",
        exports["on_tick_v4"].clone(),
    )])
    .await
    .expect("install v4");
    drop(clock);

    assert!(matches!(
        crate::resolve_live_trigger_binding("replay-cron", Some(1)),
        Err(TriggerRegistryError::UnknownBindingVersion { .. })
    ));

    event_log
        .append(
            &Topic::new(TRIGGER_EVENTS_TOPIC).expect("valid trigger events topic"),
            LogEvent::new(
                "trigger_event",
                serde_json::to_value(TriggerEventRecord {
                    binding_id: "replay-cron".to_string(),
                    binding_version: 1,
                    replay_of_event_id: None,
                    event: recorded_cron_event("evt-stale", received_at),
                })
                .expect("encode trigger event"),
            ),
        )
        .await
        .expect("append recorded event");

    let replay = vm
        .call_named_builtin(
            "trigger_replay",
            vec![VmValue::String(arcstr::ArcStr::from("evt-stale"))],
        )
        .await
        .expect("trigger replay succeeds");
    let replay: DispatchHandleRecord =
        serde_json::from_value(crate::llm::vm_value_to_json(&replay))
            .expect("decode replay handle");
    assert_eq!(replay.status, "dispatched");
    assert_eq!(replay.binding_id, "replay-cron");
    assert_eq!(replay.binding_version, 3);
    assert_eq!(replay.replay_of_event_id.as_deref(), Some("evt-stale"));

    let warning = sink
        .logs
        .borrow()
        .iter()
        .find(|log| log.category == "replay.binding_version_gc_fallback")
        .cloned()
        .expect("gc fallback warning");
    assert_eq!(warning.level, EventLevel::Warn);
    assert_eq!(
        warning.metadata.get("trigger_id"),
        Some(&serde_json::json!("replay-cron"))
    );
    assert_eq!(
        warning.metadata.get("recorded_version"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        warning.metadata.get("resolved_version"),
        Some(&serde_json::json!(3))
    );

    clear_event_sinks();
    crate::events::reset_event_sinks();
    crate::reset_thread_local_state();
}
