use super::*;

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_rate_limit_skips_excess_dispatches() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    rate_limit: Some(crate::triggers::TriggerRateLimitConfig {
                        key: None,
                        period: Duration::from_secs(60),
                        max: 1,
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-rate-1"))
                .await
                .expect("first dispatch succeeds");
            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-rate-2"))
                .await
                .expect("second dispatch returns skip");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(first[0].result, Some(serde_json::json!("delivery-rate-1")));
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "rate_limited",
                }))
            );

            let events = read_topic(
                log.clone(),
                "trigger.rate_limit.github-new-issue_v1__global",
            )
            .await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "rate_limit_allowed"));
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "rate_limit_blocked"));

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            let skipped = outbox
                .iter()
                .find(|(_, event)| event.kind == "dispatch_skipped")
                .expect("rate-limited dispatch emits skipped outbox record");
            assert_eq!(
                skipped.1.payload["skip_stage"],
                serde_json::json!("flow_control")
            );
            assert_eq!(
                skipped.1.payload["detail"]["flow_control"],
                serde_json::json!("rate_limited")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_throttle_waits_for_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let clock = crate::triggers::test_util::clock::MockClock::new(
                time::OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
            );
            let _guard = crate::triggers::test_util::clock::install_override(clock.clone());
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    throttle: Some(crate::triggers::TriggerThrottleConfig {
                        key: None,
                        period: Duration::from_secs(30),
                        max: 1,
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-throttle-1"))
                .await
                .expect("first dispatch succeeds");
            assert_eq!(first[0].status, DispatchStatus::Succeeded);

            let dispatcher_for_task = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-throttle-2"))
                    .await
                    .expect("second dispatch succeeds")
            });

            tokio::task::yield_now().await;
            assert!(
                !second.is_finished(),
                "second dispatch should still be waiting on the throttle window"
            );

            clock.advance_std(Duration::from_secs(30)).await;

            let second = second.await.expect("join throttled dispatch");
            assert_eq!(second[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!("delivery-throttle-2"))
            );

            let events =
                read_topic(log.clone(), "trigger.throttle.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "throttle_wait"));
            assert!(
                events
                    .iter()
                    .filter(|(_, event)| event.kind == "throttle_acquired")
                    .count()
                    >= 2
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_singleton_skips_while_inflight() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn slow_handler(event: TriggerEvent) -> string {
  sleep(50)
  return event.dedupe_key
}
"#,
                "slow_handler",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    singleton: Some(crate::triggers::TriggerSingletonConfig { key: None }),
                    ..Default::default()
                },
            )
            .await;

            let dispatcher_for_task = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-singleton-1"))
                    .await
                    .expect("first dispatch succeeds")
            });

            wait_for_dispatcher_in_flight(&dispatcher, 1).await;

            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-singleton-2"))
                .await
                .expect("second dispatch returns skip");
            tokio::time::advance(Duration::from_millis(50)).await;
            let first = first.await.expect("join singleton leader");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "singleton_active",
                }))
            );

            let events =
                read_topic(log.clone(), "trigger.singleton.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "singleton_acquired"));
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "singleton_skipped"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn waitpoint_wait_releases_singleton_flow_control_while_waiting() {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let state = Arc::new(super::DispatcherRuntimeState::new(log.clone()));
    let gate = "singleton-demo".to_string();

    state
        .flow_control
        .acquire_singleton(&gate)
        .await
        .expect("initial singleton acquisition");
    let acquired = Arc::new(tokio::sync::Mutex::new(super::AcquiredFlowControl {
        singleton: Some(super::SingletonLease {
            gate: gate.clone(),
            held: true,
        }),
        ..Default::default()
    }));
    let lease = super::DispatchWaitLease::new(state.clone(), acquired.clone());

    lease.suspend().await.expect("suspend releases singleton");
    assert!(
        !acquired
            .lock()
            .await
            .singleton
            .as_ref()
            .expect("singleton lease")
            .held
    );

    assert!(state
        .flow_control
        .try_acquire_singleton(&gate)
        .await
        .expect("competing dispatch can acquire while suspended"));
    state
        .flow_control
        .release_singleton(&gate)
        .await
        .expect("release competing dispatch");

    lease.resume().await.expect("resume reacquires singleton");
    assert!(
        acquired
            .lock()
            .await
            .singleton
            .as_ref()
            .expect("singleton lease")
            .held
    );
    state
        .flow_control
        .release_singleton(&gate)
        .await
        .expect("final release");

    let event_kinds = read_topic(log.clone(), "trigger.singleton.singleton-demo")
        .await
        .into_iter()
        .map(|(_, event)| event.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        event_kinds,
        vec![
            "singleton_acquired".to_string(),
            "singleton_released".to_string(),
            "singleton_acquired".to_string(),
            "singleton_released".to_string(),
            "singleton_acquired".to_string(),
            "singleton_released".to_string(),
        ]
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn monitor_wait_releases_singleton_flow_control_while_waiting() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"
import { wait_for } from "std/monitors"

pub fn coordinated_handler(event: TriggerEvent) -> string {
  if event.dedupe_key == "delivery-monitor-wait-1" {
    let result = wait_for({
      wait_id: "monitor-singleton",
      timeout: 500ms,
      poll_interval: 1h,
      source: {label: "monitor-singleton", poll: { ctx ->
        return {
          ready: ctx.last_push_event?.payload?.event?.dedupe_key == "delivery-monitor-wait-2"
        }
      }, prefers_push: true, push_filter: { event ->
        event.payload.event.dedupe_key == "delivery-monitor-wait-2"
      }},
      condition: { state -> state.ready },
    })
    return "first:" + result.status
  }
  return "second:completed"
}
"#,
                "coordinated_handler",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    singleton: Some(crate::triggers::TriggerSingletonConfig { key: None }),
                    ..Default::default()
                },
            )
            .await;

            let singleton_topic =
                Topic::new("trigger.singleton.github-new-issue_v1__global").unwrap();
            let mut singleton_events = log
                .clone()
                .subscribe(&singleton_topic, None)
                .await
                .expect("subscribe singleton events");
            let dispatcher_for_task = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                dispatcher_for_task
                    .dispatch_event(trigger_event("issues.opened", "delivery-monitor-wait-1"))
                    .await
                    .expect("first dispatch succeeds")
            });

            while let Some(event) = singleton_events.next().await {
                let (_, event) = event.expect("singleton event");
                if event.kind == "singleton_released" {
                    break;
                }
            }

            let second_event = trigger_event("issues.opened", "delivery-monitor-wait-2");
            dispatcher
                .enqueue(second_event.clone())
                .await
                .expect("enqueue second event for monitor push wakeup");
            let second = dispatcher
                .dispatch_event(second_event)
                .await
                .expect("second dispatch completes");
            let first = first.await.expect("join waiting monitor leader");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(second[0].status, DispatchStatus::Succeeded);
            assert_eq!(first[0].result, Some(serde_json::json!("first:matched")));
            assert_eq!(
                second[0].result,
                Some(serde_json::json!("second:completed"))
            );

            let events =
                read_topic(log.clone(), "trigger.singleton.github-new-issue_v1__global").await;
            let event_kinds = events
                .into_iter()
                .map(|(_, event)| event.kind)
                .collect::<Vec<_>>();
            assert_eq!(
                event_kinds,
                vec![
                    "singleton_acquired".to_string(),
                    "singleton_released".to_string(),
                    "singleton_acquired".to_string(),
                    "singleton_released".to_string(),
                    "singleton_acquired".to_string(),
                    "singleton_released".to_string(),
                ]
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_debounce_keeps_latest_event() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let clock = crate::triggers::test_util::clock::MockClock::new(
                time::OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
            );
            let _guard = crate::triggers::test_util::clock::install_override(clock.clone());
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_default_for_base_dir(dir.path()).expect("install event log");
            let lib_path = dir.path().join("lib.harn");
            std::fs::write(
                &lib_path,
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return event.dedupe_key
}
"#,
            )
            .expect("write module source");

            let mut vm = Vm::new();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            let flow_control = crate::triggers::TriggerFlowControlConfig {
                debounce: Some(crate::triggers::TriggerDebounceConfig {
                    key: compile_trigger_expr(
                        &mut vm,
                        dir.path(),
                        "debounce_group",
                        "event.headers.group",
                    )
                    .await,
                    period: Duration::from_secs(30),
                }),
                ..Default::default()
            };
            let exports = vm
                .load_module_exports(&lib_path)
                .await
                .expect("load handler exports");
            let handler = exports["local_fn"].clone();
            install_manifest_triggers(vec![TriggerBindingSpec {
                id: "github-new-issue".to_string(),
                source: TriggerBindingSource::Manifest,
                kind: "webhook".to_string(),
                provider: ProviderId::from("github"),
                autonomy_tier: crate::AutonomyTier::ActAuto,
                handler: TriggerHandlerSpec::Local {
                    raw: "local_fn".to_string(),
                    closure: handler,
                },
                dispatch_priority: crate::WorkerQueuePriority::Normal,
                when: None,
                when_budget: None,
                retry: TriggerRetryConfig::default(),
                match_events: vec!["issues.opened".to_string()],
                dedupe_key: Some("event.dedupe_key".to_string()),
                dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
                filter: None,
                daily_cost_usd: None,
                hourly_cost_usd: None,
                max_autonomous_decisions_per_hour: None,
                max_autonomous_decisions_per_day: None,
                on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
                max_concurrent: None,
                flow_control,
                manifest_path: None,
                package_name: Some("workspace".to_string()),
                definition_fingerprint: "fp:local_fn".to_string(),
            }])
            .await
            .expect("install test trigger binding");
            let dispatcher = Dispatcher::with_event_log(vm, log);

            let mut first_event = trigger_event("issues.opened", "delivery-debounce-1");
            first_event
                .headers
                .insert("group".to_string(), "issues".to_string());
            let mut second_event = trigger_event("issues.opened", "delivery-debounce-2");
            second_event
                .headers
                .insert("group".to_string(), "issues".to_string());

            let first_dispatcher = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                first_dispatcher
                    .dispatch_event(first_event)
                    .await
                    .expect("first dispatch completes")
            });
            tokio::task::yield_now().await;

            let second_dispatcher = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                second_dispatcher
                    .dispatch_event(second_event)
                    .await
                    .expect("second dispatch completes")
            });
            tokio::task::yield_now().await;

            clock.advance_std(Duration::from_secs(30)).await;

            let first = first.await.expect("join first debounce dispatch");
            let second = second.await.expect("join second debounce dispatch");
            assert_eq!(first[0].status, DispatchStatus::Skipped);
            assert_eq!(second[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!("delivery-debounce-2"))
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_batch_coalesces_multiple_events() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_flow_control(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  let batch_count = if event.batch == nil { 0 } else { len(event.batch) }
  return {dedupe_key: event.dedupe_key, batch_count: batch_count}
}
"#,
                "local_fn",
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig {
                    batch: Some(crate::triggers::TriggerBatchConfig {
                        key: None,
                        size: 2,
                        timeout: Duration::from_secs(30),
                    }),
                    ..Default::default()
                },
            )
            .await;

            let first_dispatcher = dispatcher.clone();
            let first = tokio::task::spawn_local(async move {
                first_dispatcher
                    .dispatch_event(trigger_event("issues.opened", "delivery-batch-1"))
                    .await
                    .expect("first batch dispatch completes")
            });
            tokio::task::yield_now().await;

            let second_dispatcher = dispatcher.clone();
            let second = tokio::task::spawn_local(async move {
                second_dispatcher
                    .dispatch_event(trigger_event("issues.opened", "delivery-batch-2"))
                    .await
                    .expect("second batch dispatch completes")
            });

            let first = first.await.expect("join batch leader");
            let second = second.await.expect("join batch follower");

            assert_eq!(first[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                first[0].result,
                Some(serde_json::json!({
                    "dedupe_key": "delivery-batch-1",
                    "batch_count": 2,
                }))
            );
            assert_eq!(second[0].status, DispatchStatus::Skipped);
            assert_eq!(
                second[0].result,
                Some(serde_json::json!({
                    "skipped": true,
                    "flow_control": "batch_merged",
                }))
            );

            let events = read_topic(log.clone(), "trigger.batch.github-new-issue_v1__global").await;
            assert!(events
                .iter()
                .any(|(_, event)| event.kind == "batch_dispatched"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flow_control_priority_prefers_higher_ranked_waiters() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let log = install_default_for_base_dir(dir.path()).expect("install event log");
            let lib_path = dir.path().join("lib.harn");
            std::fs::write(
                &lib_path,
                r#"
import "std/triggers"

pub fn slow_handler(event: TriggerEvent) -> string {
  sleep(30)
  return event.headers.tier
}
"#,
            )
            .expect("write module source");

            let mut vm = Vm::new();
            register_vm_stdlib(&mut vm);
            vm.set_source_dir(dir.path());
            let flow_control = crate::triggers::TriggerFlowControlConfig {
                concurrency: Some(crate::triggers::TriggerConcurrencyConfig { key: None, max: 1 }),
                priority: Some(crate::triggers::TriggerPriorityOrderConfig {
                    key: compile_trigger_expr(
                        &mut vm,
                        dir.path(),
                        "priority_tier",
                        "event.headers.tier",
                    )
                    .await,
                    order: vec![
                        "gold".to_string(),
                        "silver".to_string(),
                        "bronze".to_string(),
                    ],
                }),
                ..Default::default()
            };
            let exports = vm
                .load_module_exports(&lib_path)
                .await
                .expect("load handler exports");
            let handler = exports["slow_handler"].clone();
            install_manifest_triggers(vec![TriggerBindingSpec {
                id: "github-new-issue".to_string(),
                source: TriggerBindingSource::Manifest,
                kind: "webhook".to_string(),
                provider: ProviderId::from("github"),
                autonomy_tier: crate::AutonomyTier::ActAuto,
                handler: TriggerHandlerSpec::Local {
                    raw: "slow_handler".to_string(),
                    closure: handler,
                },
                dispatch_priority: crate::WorkerQueuePriority::Normal,
                when: None,
                when_budget: None,
                retry: TriggerRetryConfig::default(),
                match_events: vec!["issues.opened".to_string()],
                dedupe_key: Some("event.dedupe_key".to_string()),
                dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
                filter: None,
                daily_cost_usd: None,
                hourly_cost_usd: None,
                max_autonomous_decisions_per_hour: None,
                max_autonomous_decisions_per_day: None,
                on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
                max_concurrent: None,
                flow_control,
                manifest_path: None,
                package_name: Some("workspace".to_string()),
                definition_fingerprint: "fp:slow_handler".to_string(),
            }])
            .await
            .expect("install test trigger binding");
            let dispatcher = Dispatcher::with_event_log(vm, log.clone());

            let mut bronze_first = trigger_event("issues.opened", "delivery-priority-bronze-1");
            bronze_first
                .headers
                .insert("tier".to_string(), "bronze".to_string());
            let mut bronze_second = trigger_event("issues.opened", "delivery-priority-bronze-2");
            bronze_second
                .headers
                .insert("tier".to_string(), "bronze".to_string());
            let mut gold = trigger_event("issues.opened", "delivery-priority-gold");
            gold.headers.insert("tier".to_string(), "gold".to_string());

            let bronze_first_id = bronze_first.id.0.clone();
            let bronze_second_id = bronze_second.id.0.clone();
            let gold_id = gold.id.0.clone();

            let leader_dispatcher = dispatcher.clone();
            let leader = tokio::task::spawn_local(async move {
                leader_dispatcher
                    .dispatch_event(bronze_first)
                    .await
                    .expect("leader dispatch succeeds")
            });

            wait_for_dispatcher_in_flight(&dispatcher, 1).await;

            let bronze_dispatcher = dispatcher.clone();
            let bronze_waiter = tokio::task::spawn_local(async move {
                bronze_dispatcher
                    .dispatch_event(bronze_second)
                    .await
                    .expect("bronze waiter succeeds")
            });
            let gold_dispatcher = dispatcher.clone();
            let gold_waiter = tokio::task::spawn_local(async move {
                gold_dispatcher
                    .dispatch_event(gold)
                    .await
                    .expect("gold waiter succeeds")
            });

            tokio::time::advance(Duration::from_millis(30)).await;
            tokio::time::advance(Duration::from_millis(30)).await;
            tokio::time::advance(Duration::from_millis(30)).await;

            let leader = leader.await.expect("join leader");
            let gold = gold_waiter.await.expect("join gold waiter");
            let bronze = bronze_waiter.await.expect("join bronze waiter");
            assert_eq!(leader[0].status, DispatchStatus::Succeeded);
            assert_eq!(gold[0].status, DispatchStatus::Succeeded);
            assert_eq!(bronze[0].status, DispatchStatus::Succeeded);

            let started = read_topic(log.clone(), "trigger.outbox")
                .await
                .into_iter()
                .filter(|(_, event)| event.kind == "dispatch_started")
                .filter_map(|(_, event)| event.headers.get("event_id").cloned())
                .filter(|event_id| {
                    event_id == &bronze_first_id
                        || event_id == &bronze_second_id
                        || event_id == &gold_id
                })
                .collect::<Vec<_>>();
            assert_eq!(started, vec![bronze_first_id, gold_id, bronze_second_id]);
        })
        .await;
}
