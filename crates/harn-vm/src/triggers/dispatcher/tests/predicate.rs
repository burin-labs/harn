use super::*;

#[tokio::test(flavor = "current_thread")]
async fn predicate_budget_exceeded_short_circuits_and_emits_lifecycle() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_options(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "budget gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini"},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                Some(TriggerPredicateBudget {
                    max_cost_usd: Some(0.001),
                    tokens_max: Some(1),
                    timeout_ms: None,
                }),
                None,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(3_000),
                output_tokens: Some(4_000),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                thinking_summary: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                logprobs: Vec::new(),
                error: None,
                stream_chunks: Vec::new(),
            });

            let outcome = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-budget"))
                .await
                .expect("dispatch succeeds")
                .into_iter()
                .next()
                .expect("dispatch outcome");

            assert_eq!(outcome.status, DispatchStatus::Skipped);
            assert_eq!(
                outcome
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("budget_exceeded")
            );

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let budget_events = lifecycle_payloads(&lifecycle, "predicate.budget_exceeded");
            assert_eq!(budget_events.len(), 1);
            assert!(budget_events[0]["cost_usd"].as_f64().unwrap_or_default() > 0.001);
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 1);
            assert_eq!(evaluated[0]["result"], serde_json::json!(false));
            assert_eq!(evaluated[0]["reason"], serde_json::json!("budget_exceeded"));

            let outbox = read_topic(log.clone(), "trigger.outbox").await;
            assert!(!outbox
                .iter()
                .any(|(_, event)| event.kind == "dispatch_started"));
            assert_eq!(get_llm_mock_calls().len(), 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_daily_budget_exceeded_short_circuits_subsequent_events() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_options(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "daily gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini"},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                None,
                Some(0.001),
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(3_000),
                output_tokens: Some(4_000),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                thinking_summary: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                logprobs: Vec::new(),
                error: None,
                stream_chunks: Vec::new(),
            });

            let first = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-daily-1"))
                .await
                .expect("first dispatch succeeds")
                .into_iter()
                .next()
                .expect("first outcome");
            assert_eq!(first.status, DispatchStatus::Skipped);
            assert_eq!(
                first
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("daily_budget_exceeded")
            );

            let second = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-daily-2"))
                .await
                .expect("second dispatch succeeds")
                .into_iter()
                .next()
                .expect("second outcome");
            assert_eq!(second.status, DispatchStatus::Skipped);
            assert_eq!(
                second
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("daily_budget_exceeded")
            );
            assert_eq!(get_llm_mock_calls().len(), 1);

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let daily_events = lifecycle_payloads(&lifecycle, "predicate.daily_budget_exceeded");
            assert_eq!(daily_events.len(), 2);
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 2);
            assert_eq!(
                evaluated[0]["reason"],
                serde_json::json!("daily_budget_exceeded")
            );
            assert_eq!(
                evaluated[1]["reason"],
                serde_json::json!("daily_budget_exceeded")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_budget_warn_strategy_proceeds_without_llm_spend() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_budget_strategy(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "warn gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini"},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                Some(TriggerPredicateBudget {
                    max_cost_usd: Some(0.001),
                    tokens_max: None,
                    timeout_ms: None,
                }),
                Some(0.0),
                None,
                crate::TriggerBudgetExhaustionStrategy::Warn,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            let outcome = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-budget-warn"))
                .await
                .expect("dispatch succeeds")
                .into_iter()
                .next()
                .expect("dispatch outcome");

            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(get_llm_mock_calls().len(), 0);
            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated[0]["result"], serde_json::json!(true));
            assert_eq!(
                evaluated[0]["on_budget_exhausted"],
                serde_json::json!("warn")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_budget_fail_strategy_moves_to_dlq() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_budget_strategy(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  return true
}
"#,
                "local_fn",
                Some("should_handle"),
                Some(TriggerPredicateBudget {
                    max_cost_usd: Some(0.001),
                    tokens_max: None,
                    timeout_ms: None,
                }),
                Some(0.0),
                None,
                crate::TriggerBudgetExhaustionStrategy::Fail,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            let outcome = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-budget-fail"))
                .await
                .expect("dispatch succeeds")
                .into_iter()
                .next()
                .expect("dispatch outcome");

            assert_eq!(outcome.status, DispatchStatus::Dlq);
            assert_eq!(dispatcher.dlq_entries().len(), 1);
            let dlq = read_topic(log.clone(), "trigger.dlq").await;
            assert_eq!(dlq.len(), 1);
            assert_eq!(dlq[0].1.kind, "dlq_moved");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_budget_retry_later_strategy_defers_event() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture_with_budget_strategy(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  return true
}
"#,
                "local_fn",
                Some("should_handle"),
                Some(TriggerPredicateBudget {
                    max_cost_usd: Some(0.001),
                    tokens_max: None,
                    timeout_ms: None,
                }),
                Some(0.0),
                None,
                crate::TriggerBudgetExhaustionStrategy::RetryLater,
                TriggerRetryConfig::default(),
                crate::triggers::TriggerFlowControlConfig::default(),
            )
            .await;

            let outcome = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-budget-retry"))
                .await
                .expect("dispatch succeeds")
                .into_iter()
                .next()
                .expect("dispatch outcome");

            assert_eq!(outcome.status, DispatchStatus::Waiting);
            let attempts = read_topic(log.clone(), "trigger.attempts").await;
            assert!(attempts
                .iter()
                .any(|(_, event)| event.kind == "budget_deferred"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_replay_uses_event_cache_without_hitting_provider() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (_dir, log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  let result = llm_call(
    "replay gate " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini"},
  )
  return contains(result.text, "yes")
}
"#,
                "local_fn",
                Some("should_handle"),
                TriggerRetryConfig::default(),
            )
            .await;

            push_llm_mock(LlmMock {
                text: "yes".to_string(),
                tool_calls: Vec::new(),
                match_pattern: None,
                consume_on_match: false,
                input_tokens: Some(10),
                output_tokens: Some(5),
                cache_read_tokens: None,
                cache_write_tokens: None,
                thinking: None,
                thinking_summary: None,
                stop_reason: None,
                model: "gpt-4o-mini".to_string(),
                provider: Some("mock".to_string()),
                blocks: None,
                logprobs: Vec::new(),
                error: None,
                stream_chunks: Vec::new(),
            });

            let event = trigger_event("issues.opened", "delivery-replay-cache");
            let first = dispatcher
                .dispatch_event(event.clone())
                .await
                .expect("first dispatch succeeds")
                .into_iter()
                .next()
                .expect("first outcome");
            assert_eq!(first.status, DispatchStatus::Succeeded);
            assert_eq!(get_llm_mock_calls().len(), 1);

            crate::llm::reset_llm_state();

            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let replay = dispatcher
                .dispatch_replay(&binding, event.clone(), event.id.0.clone())
                .await
                .expect("replay succeeds");
            assert_eq!(replay.status, DispatchStatus::Succeeded);
            assert!(get_llm_mock_calls().is_empty());

            let lifecycle = read_topic(log.clone(), "triggers.lifecycle").await;
            let evaluated = lifecycle_payloads(&lifecycle, "predicate.evaluated");
            assert_eq!(evaluated.len(), 2);
            assert_eq!(evaluated[0]["cached"], serde_json::json!(false));
            assert_eq!(evaluated[1]["cached"], serde_json::json!(true));
            assert_eq!(
                evaluated[1]["replay_of_event_id"],
                serde_json::json!(event.id.0)
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn predicate_circuit_breaker_opens_after_three_failures() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let sink = Rc::new(CollectorSink::new());
            let (_dir, _log, dispatcher) = dispatcher_fixture(
                r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> string {
  return "handled:" + event.kind
}

pub fn should_handle(event: TriggerEvent) -> bool {
  throw "predicate failed"
}
"#,
                "local_fn",
                Some("should_handle"),
                TriggerRetryConfig::default(),
            )
            .await;
            clear_event_sinks();
            add_event_sink(sink.clone());

            for index in 0..3 {
                let outcome = dispatcher
                    .dispatch_event(trigger_event(
                        "issues.opened",
                        &format!("delivery-circuit-{index}"),
                    ))
                    .await
                    .expect("dispatch succeeds")
                    .into_iter()
                    .next()
                    .expect("dispatch outcome");
                assert_eq!(outcome.status, DispatchStatus::Skipped);
            }

            let fourth = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-circuit-4"))
                .await
                .expect("fourth dispatch succeeds")
                .into_iter()
                .next()
                .expect("fourth outcome");
            assert_eq!(fourth.status, DispatchStatus::Skipped);
            assert_eq!(
                fourth
                    .result
                    .as_ref()
                    .and_then(|result| result["reason"].as_str()),
                Some("circuit_open")
            );
            let binding =
                resolve_live_trigger_binding("github-new-issue", None).expect("resolve binding");
            let state = binding
                .predicate_state
                .lock()
                .expect("predicate state lock");
            assert_eq!(state.consecutive_failures, 3);
            assert!(state.breaker_open_until_ms.is_some());

            let logs = sink.logs.borrow();
            assert!(logs.iter().any(|event| {
                event.level == EventLevel::Warn
                    && event.category == "trigger.predicate.circuit_breaker"
                    && event.message.contains("opened for 5 minutes")
            }));
            assert!(logs.iter().any(|event| {
                event.level == EventLevel::Warn
                    && event.category == "trigger.predicate.circuit_breaker"
                    && event.message.contains("short-circuiting to false")
            }));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn autonomy_budget_routes_act_auto_to_approval() {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let lib_path = dir.path().join("lib.harn");
    std::fs::write(
        &lib_path,
        r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) -> dict {
  return {ok: true, event_id: event.id}
}
"#,
    )
    .expect("write module source");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let exports = vm
        .load_module_exports(&lib_path)
        .await
        .expect("load handler exports");
    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-new-issue".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: "local_fn".to_string(),
            closure: exports["local_fn"].clone(),
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
        max_autonomous_decisions_per_day: Some(1),
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: "fp:autonomy-budget".to_string(),
    }])
    .await
    .expect("install trigger binding");
    let dispatcher = Dispatcher::with_event_log(vm, log.clone());

    let first = dispatcher
        .dispatch_event(trigger_event("issues.opened", "delivery-auto-1"))
        .await
        .expect("first dispatch succeeds");
    assert_eq!(first[0].status, DispatchStatus::Succeeded);

    let second = dispatcher
        .dispatch_event(trigger_event("issues.opened", "delivery-auto-2"))
        .await
        .expect("second dispatch waits for approval");
    assert_eq!(second[0].status, DispatchStatus::Waiting);
    let result = second[0].result.as_ref().expect("approval result");
    assert_eq!(result["approval_required"], true);
    assert_eq!(result["reason"], "daily_autonomy_budget_exceeded");

    let approvals = read_topic(log.clone(), crate::HITL_APPROVALS_TOPIC).await;
    assert_eq!(approvals.len(), 1);
    assert_eq!(
        approvals[0].1.payload["payload"]["reviewers"][0],
        super::DEFAULT_AUTONOMY_BUDGET_REVIEWER
    );

    let lifecycle = read_topic(log.clone(), crate::TRIGGERS_LIFECYCLE_TOPIC).await;
    assert!(lifecycle
        .iter()
        .any(|(_, event)| event.kind == "autonomy.budget_exceeded"));

    let action_graph = read_topic(log.clone(), "observability.action_graph").await;
    let (node_kinds, edge_kinds) = flatten_action_graph(&action_graph);
    assert!(node_kinds.iter().any(|kind| kind == "approval"));
    assert!(edge_kinds.iter().any(|kind| kind == "approval_gate"));

    let trust_records = crate::query_trust_records(
        &log,
        &crate::TrustQueryFilters {
            agent: Some("github-new-issue".to_string()),
            action: Some("autonomy.tier_transition".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("query trust records");
    assert_eq!(trust_records.len(), 1);
    assert_eq!(
        trust_records[0].metadata["to_tier"],
        crate::AutonomyTier::ActWithApproval.as_str()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn handler_tier_is_enforced_through_capability_policy() {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
    let lib_path = dir.path().join("lib.harn");
    std::fs::write(
        &lib_path,
        r#"
import "std/triggers"

pub fn local_fn(event: TriggerEvent) {
  write_file(path_join(temp_dir(), "blocked.txt"), "blocked")
}
"#,
    )
    .expect("write module source");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let exports = vm
        .load_module_exports(&lib_path)
        .await
        .expect("load handler exports");
    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-new-issue".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::Suggest,
        handler: TriggerHandlerSpec::Local {
            raw: "local_fn".to_string(),
            closure: exports["local_fn"].clone(),
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
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: "fp:tier-policy".to_string(),
    }])
    .await
    .expect("install trigger binding");
    let dispatcher = Dispatcher::with_event_log(vm, log.clone());

    let outcomes = dispatcher
        .dispatch_event(trigger_event("issues.opened", "delivery-suggest-1"))
        .await
        .expect("dispatch completes with side effect proposal");
    assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
    assert!(outcomes[0].error.is_none());

    let outbox = read_topic(log.clone(), crate::TRIGGER_OUTBOX_TOPIC).await;
    assert!(outbox
        .iter()
        .any(|(_, event)| event.kind == "dispatch_proposed"));
}

#[test]
fn uri_parser_rejects_invalid_and_unknown_handler_schemes() {
    assert_eq!(DispatchUri::parse("").unwrap_err(), DispatchUriError::Empty);
    assert_eq!(
        DispatchUri::parse("a2a://").unwrap_err(),
        DispatchUriError::MissingTarget {
            scheme: "a2a".to_string()
        }
    );
    assert_eq!(
        DispatchUri::parse("worker://").unwrap_err(),
        DispatchUriError::MissingTarget {
            scheme: "worker".to_string()
        }
    );
    assert_eq!(
        DispatchUri::parse("smtp://relay").unwrap_err(),
        DispatchUriError::UnknownScheme("smtp".to_string())
    );
}
