use super::*;
use crate::event_log::{
    install_active_event_log, install_memory_for_current_thread, open_event_log,
    pin_test_occurred_at_ms, reset_active_event_log, EventLogBackendKind, EventLogConfig,
};
use crate::events::{add_event_sink, clear_event_sinks, CollectorSink, EventLevel};
use std::path::Path;
use std::rc::Rc;

/// Build the `OffsetDateTime` that corresponds to a pinned event-log
/// `occurred_at_ms`, so `as_of` cutoffs can be expressed in the same
/// reference frame as `pin_test_occurred_at_ms` without touching the wall
/// clock.
fn offset_from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000)
        .expect("epoch ms within OffsetDateTime range")
}

fn manifest_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
    TriggerBindingSpec {
        id: id.to_string(),
        source: TriggerBindingSource::Manifest,
        ingress_subscription: false,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Worker {
            queue: format!("{id}-queue"),
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: TriggerRetryConfig::default(),
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: Some("event.kind".to_string()),
        daily_cost_usd: Some(5.0),
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: Some(10),
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: fingerprint.to_string(),
    }
}

fn install_test_memory_event_log() -> Arc<AnyEventLog> {
    install_memory_for_current_thread(512)
}

fn install_test_sqlite_event_log(base_dir: &Path) -> Arc<AnyEventLog> {
    let config = EventLogConfig {
        backend: EventLogBackendKind::Sqlite,
        file_dir: base_dir.join("events"),
        sqlite_path: base_dir.join("events.sqlite"),
        queue_depth: 512,
    };
    let log = open_event_log(&config).expect("open isolated sqlite event log");
    install_active_event_log(log)
}

fn dynamic_spec(id: &str) -> TriggerBindingSpec {
    TriggerBindingSpec {
        id: id.to_string(),
        source: TriggerBindingSource::Dynamic,
        ingress_subscription: false,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Worker {
            queue: format!("{id}-queue"),
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry: TriggerRetryConfig::default(),
        match_events: vec!["issues.opened".to_string()],
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
        package_name: None,
        definition_fingerprint: format!("dynamic:{id}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn manifest_loaded_trigger_registers_with_zeroed_metrics() {
    clear_trigger_registry();

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("manifest trigger installs");

    let snapshots = snapshot_trigger_bindings();
    assert_eq!(snapshots.len(), 1);
    let binding = &snapshots[0];
    assert_eq!(binding.id, "github-new-issue");
    assert_eq!(binding.version, 1);
    assert_eq!(binding.state, TriggerState::Active);
    assert_eq!(binding.metrics, TriggerMetricsSnapshot::default());

    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_register_assigns_unique_ids_and_rejects_duplicates() {
    clear_trigger_registry();

    let first = dynamic_register(dynamic_spec("dynamic-a"))
        .await
        .expect("first dynamic trigger");
    let second = dynamic_register(dynamic_spec("dynamic-b"))
        .await
        .expect("second dynamic trigger");
    assert_ne!(first, second);

    let error = dynamic_register(dynamic_spec("dynamic-a"))
        .await
        .expect_err("duplicate id should fail");
    assert!(matches!(error, TriggerRegistryError::DuplicateId(_)));

    clear_trigger_registry();
}

#[test]
fn expected_predicate_cost_average_does_not_overflow() {
    let binding = TriggerBinding::new(manifest_spec("costed", "v1"), 1);
    record_predicate_cost_sample(&binding, u64::MAX);
    record_predicate_cost_sample(&binding, u64::MAX);

    assert_eq!(expected_predicate_cost_usd_micros(&binding), u64::MAX);
}

#[tokio::test(flavor = "current_thread")]
async fn drain_waits_for_in_flight_events_before_terminating() {
    clear_trigger_registry();

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("manifest trigger installs");
    begin_in_flight("github-new-issue", 1).expect("start in-flight event");

    drain("github-new-issue").await.expect("drain succeeds");
    let binding = snapshot_trigger_bindings()
        .into_iter()
        .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
        .expect("binding snapshot");
    assert_eq!(binding.state, TriggerState::Draining);
    assert_eq!(binding.metrics.in_flight, 1);

    finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
        .await
        .expect("finish in-flight event");
    let binding = snapshot_trigger_bindings()
        .into_iter()
        .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
        .expect("binding snapshot");
    assert_eq!(binding.state, TriggerState::Terminated);
    assert_eq!(binding.metrics.in_flight, 0);

    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn hot_reload_registers_new_version_while_old_binding_drains() {
    clear_trigger_registry();

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("initial manifest trigger installs");
    begin_in_flight("github-new-issue", 1).expect("start in-flight event");

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("updated manifest trigger installs");

    let snapshots = snapshot_trigger_bindings();
    assert_eq!(snapshots.len(), 2);
    let old = snapshots
        .iter()
        .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
        .expect("old binding");
    let new = snapshots
        .iter()
        .find(|binding| binding.id == "github-new-issue" && binding.version == 2)
        .expect("new binding");
    assert_eq!(old.state, TriggerState::Draining);
    assert_eq!(new.state, TriggerState::Active);

    finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
        .await
        .expect("finish old in-flight event");
    let old = snapshot_trigger_bindings()
        .into_iter()
        .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
        .expect("old binding");
    assert_eq!(old.state, TriggerState::Terminated);

    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn restoring_a_draining_definition_keeps_in_flight_generations_distinct() {
    clear_trigger_registry();
    reset_active_event_log();
    let tempdir = tempfile::tempdir().unwrap();
    install_test_sqlite_event_log(tempdir.path());
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .unwrap();
    begin_in_flight("github-new-issue", 1).unwrap();
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .unwrap();
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .unwrap();
    let restored = resolve_live_trigger_binding("github-new-issue", None).unwrap();
    assert_eq!(restored.version, 3);
    begin_in_flight("github-new-issue", 3).unwrap();
    finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
        .await
        .unwrap();
    assert_eq!(restored.metrics_snapshot().in_flight, 1);
    assert_eq!(restored.state_snapshot(), TriggerState::Active);
    finish_in_flight("github-new-issue", 3, TriggerDispatchOutcome::Dispatched)
        .await
        .unwrap();
    assert_eq!(restored.metrics_snapshot().dispatched, 1);
    clear_trigger_registry();
    reset_active_event_log();
    install_test_sqlite_event_log(tempdir.path());
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .unwrap();
    assert_eq!(
        resolve_live_trigger_binding("github-new-issue", None)
            .unwrap()
            .version,
        3
    );
    reset_active_event_log();
    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn gc_drops_terminated_versions_beyond_retention_limit() {
    clear_trigger_registry();

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("install v1");
    begin_in_flight("github-new-issue", 1).expect("pin v1");

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("install v2");
    finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
        .await
        .expect("finish v1");

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v3")])
        .await
        .expect("install v3");

    let snapshots = snapshot_trigger_bindings();
    let versions: Vec<u32> = snapshots
        .into_iter()
        .filter(|binding| binding.id == "github-new-issue")
        .map(|binding| binding.version)
        .collect();
    assert_eq!(versions, vec![2, 3]);

    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn current_binding_prefers_restored_definition_over_newer_history() {
    clear_trigger_registry();
    reset_active_event_log();
    install_test_memory_event_log();
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .unwrap();
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .unwrap();
    begin_in_flight("github-new-issue", 2).unwrap();
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .unwrap();
    let restored = resolve_live_trigger_binding("github-new-issue", None).unwrap();
    assert_eq!(restored.version, 1);
    assert_eq!(current_trigger_binding_snapshots().len(), 1);
    assert_eq!(current_trigger_binding_snapshots()[0].version, 1);
    assert!(snapshot_trigger_bindings()
        .iter()
        .any(|binding| binding.version == 2 && binding.state == TriggerState::Draining));
    finish_in_flight("github-new-issue", 2, TriggerDispatchOutcome::Dispatched)
        .await
        .unwrap();
    let current = current_trigger_binding_snapshots();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].version, 1);
    assert_eq!(current[0].state, TriggerState::Active);
    assert_eq!(
        resolve_live_trigger_binding("github-new-issue", None)
            .unwrap()
            .version,
        1
    );
    reset_active_event_log();
    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_transitions_append_to_event_log() {
    clear_trigger_registry();
    reset_active_event_log();
    let log = install_test_memory_event_log();

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("manifest trigger installs");
    begin_in_flight("github-new-issue", 1).expect("start in-flight event");
    drain("github-new-issue").await.expect("drain succeeds");
    finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
        .await
        .expect("finish event");

    let topic = Topic::new("triggers.lifecycle").expect("valid lifecycle topic");
    let events = log
        .read_range(&topic, None, 32)
        .await
        .expect("read lifecycle events");
    let states: Vec<String> = events
        .into_iter()
        .filter_map(|(_, event)| {
            event
                .payload
                .get("to_state")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .collect();
    assert_eq!(
        states,
        vec![
            "registering".to_string(),
            "active".to_string(),
            "draining".to_string(),
            "terminated".to_string(),
        ]
    );

    reset_active_event_log();
    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn version_history_reuses_historical_version_after_restart() {
    clear_trigger_registry();
    reset_active_event_log();
    let tempdir = tempfile::tempdir().expect("tempdir");
    install_test_sqlite_event_log(tempdir.path());

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("initial manifest trigger installs");
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("updated manifest trigger installs");

    clear_trigger_registry();
    reset_active_event_log();
    install_test_sqlite_event_log(tempdir.path());

    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("manifest reload reuses historical version");

    let binding = snapshot_trigger_bindings()
        .into_iter()
        .find(|binding| binding.id == "github-new-issue")
        .expect("binding snapshot");
    assert_eq!(binding.version, 2);

    reset_active_event_log();
    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn binding_version_as_of_reports_historical_active_version() {
    clear_trigger_registry();
    reset_active_event_log();
    install_test_memory_event_log();

    // Pin the event-log timestamp instead of sleeping between captures:
    // v1's lifecycle events stamp at T1, v2's at T2 > T1, with zero
    // wall-clock dependence. `before_reload` sits at T1 (so v1 is active
    // but v2 is not yet), `after_reload` at T2 (so v2 is active).
    const T1_MS: i64 = 1_700_000_000_000;
    const T2_MS: i64 = T1_MS + 50;
    let before_reload = offset_from_ms(T1_MS);
    let after_reload = offset_from_ms(T2_MS);

    let clock = pin_test_occurred_at_ms(T1_MS);
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("initial manifest trigger installs");
    drop(clock);

    let clock = pin_test_occurred_at_ms(T2_MS);
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("updated manifest trigger installs");
    drop(clock);

    assert_eq!(
        binding_version_as_of("github-new-issue", before_reload).expect("version before reload"),
        1
    );
    assert_eq!(
        binding_version_as_of("github-new-issue", after_reload).expect("version after reload"),
        2
    );

    reset_active_event_log();
    clear_trigger_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn resolve_live_or_as_of_logs_structured_gc_fallback() {
    clear_trigger_registry();
    reset_active_event_log();
    let sink = Rc::new(CollectorSink::new());
    clear_event_sinks();
    add_event_sink(sink.clone());
    install_test_memory_event_log();

    // Pin the event-log timestamps: v1..v3 land at T1, v4 at T2 > T1, with
    // `received_at` at T1 so the gc fallback resolves to v3 (the latest
    // binding active at-or-before `received_at`) while v4 is strictly later.
    const T1_MS: i64 = 1_700_000_000_000;
    const T2_MS: i64 = T1_MS + 50;
    let received_at = offset_from_ms(T1_MS);

    let clock = pin_test_occurred_at_ms(T1_MS);
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
        .await
        .expect("install v1");
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
        .await
        .expect("install v2");
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v3")])
        .await
        .expect("install v3");
    drop(clock);

    let clock = pin_test_occurred_at_ms(T2_MS);
    install_manifest_triggers(vec![manifest_spec("github-new-issue", "v4")])
        .await
        .expect("install v4");
    drop(clock);

    let binding = resolve_live_or_as_of(
        "github-new-issue",
        RecordedTriggerBinding {
            version: 1,
            received_at,
        },
    )
    .expect("resolve fallback binding");
    assert_eq!(binding.version, 3);

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
        Some(&serde_json::json!("github-new-issue"))
    );
    assert_eq!(
        warning.metadata.get("recorded_version"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        warning.metadata.get("received_at"),
        Some(&serde_json::json!(received_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| received_at.to_string())))
    );
    assert_eq!(
        warning.metadata.get("resolved_version"),
        Some(&serde_json::json!(3))
    );

    clear_event_sinks();
    crate::events::reset_event_sinks();
    reset_active_event_log();
    clear_trigger_registry();
}
