use std::path::Path;

use super::*;

async fn persona_dispatcher(
    module_path: &Path,
    function_name: &str,
    retry: TriggerRetryConfig,
    autonomy_ceiling: crate::AutonomyTier,
    execution_policy: crate::orchestration::CapabilityPolicy,
    max_autonomous_decisions_per_day: Option<u64>,
) -> (Arc<crate::event_log::AnyEventLog>, Dispatcher) {
    let persona = crate::PersonaRuntimeBinding {
        name: "merge_captain".to_string(),
        template_ref: Some("software_factory@v0".to_string()),
        entry_workflow: format!("{}#{function_name}", module_path.display()),
        schedules: Vec::new(),
        triggers: vec!["github.issues.opened".to_string()],
        budget: crate::PersonaBudgetPolicy::default(),
        stages: Vec::new(),
    };
    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "persona.merge_captain.github.issues.opened".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Persona {
            binding: persona,
            callable: crate::value::VmCallable::Pipeline(
                crate::value::LazyPipelineCallable::new(module_path.to_path_buf(), function_name)
                    .with_execution_policy(execution_policy)
                    .with_autonomy_ceiling(autonomy_ceiling),
            ),
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day,
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: "persona-workflow-v1".to_string(),
    }])
    .await
    .expect("install persona trigger");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(module_path.parent().expect("module parent"));
    let log = install_test_event_log();
    (log.clone(), Dispatcher::with_event_log(vm, log))
}

#[tokio::test(flavor = "current_thread")]
async fn persona_dispatch_invokes_entry_workflow_before_completing_run() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("merge_captain.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

pub pipeline run(event) {
  return {executed: true, kind: event.kind, cost_usd: 0.125, tokens: 42}
}
"#,
            )
            .expect("write persona workflow");
            let (log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::default(),
                crate::AutonomyTier::ActAuto,
                crate::orchestration::CapabilityPolicy::default(),
                None,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "persona-success"))
                .await
                .expect("persona dispatch succeeds");

            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            let receipt = outcomes[0].result.as_ref().expect("persona receipt");
            assert_eq!(receipt["status"], "completed");
            assert_eq!(receipt["result"]["executed"], true);
            assert_eq!(receipt["result"]["kind"], "issues.opened");

            let lifecycle = read_topic(log, crate::personas::PERSONA_RUNTIME_TOPIC).await;
            let kinds = lifecycle
                .iter()
                .map(|(_, event)| event.kind.as_str())
                .collect::<Vec<_>>();
            let started = kinds
                .iter()
                .position(|kind| *kind == "persona.run.started")
                .expect("run started");
            let completed = kinds
                .iter()
                .position(|kind| *kind == "persona.run.completed")
                .expect("run completed");
            assert!(started < completed);
            assert!(!kinds.contains(&"persona.run.failed"));
            let budget = lifecycle
                .iter()
                .find(|(_, event)| event.kind == "persona.budget.recorded")
                .map(|(_, event)| &event.payload)
                .expect("budget receipt");
            assert_eq!(budget["cost_usd"], 0.125);
            assert_eq!(budget["tokens"], 42);
            let value = lifecycle
                .iter()
                .find(|(_, event)| event.kind == "persona.value.run_completed")
                .map(|(_, event)| &event.payload)
                .expect("completion value receipt");
            assert_eq!(value["paid_cost_usd"], 0.125);
            assert_eq!(value["llm_steps"], 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn persona_callable_autonomy_ceiling_survives_higher_dispatch_tier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("bounded_persona.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

pub pipeline run(_event) {
  return handler_context()
}
"#,
            )
            .expect("write persona workflow");
            let (_log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::default(),
                crate::AutonomyTier::Suggest,
                crate::orchestration::CapabilityPolicy::default(),
                Some(0),
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "persona-autonomy-ceiling"))
                .await
                .expect("persona dispatch succeeds");
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(
                outcomes[0].result.as_ref().unwrap()["result"]["autonomy_tier"],
                "suggest"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn persona_dispatch_enforces_explicit_deny_all_capability_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("restricted_persona.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

pub pipeline run(_event) {
  return llm_call("this call must never reach a provider", nil, {provider: "mock"})
}
"#,
            )
            .expect("write restricted persona workflow");
            let (_log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                crate::AutonomyTier::ActAuto,
                {
                    let mut policy = crate::orchestration::CapabilityPolicy::default();
                    policy.restrict_tools(Vec::new());
                    policy.restrict_capabilities(std::collections::BTreeMap::new());
                    policy
                },
                None,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "persona-policy-denial"))
                .await
                .expect("policy denial is recorded as a dispatch outcome");

            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Failed);
            assert_eq!(outcomes[0].attempt_count, 1);
            assert!(
                outcomes[0].error.as_deref().is_some_and(|error| {
                    error.contains("tool_rejected") && error.contains("llm.call ceiling")
                }),
                "{:?}",
                outcomes[0].error
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn persona_dispatch_failure_records_failed_run_and_releases_lease() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("merge_captain.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

pub pipeline run(_event) {
  throw "persona failed"
}
"#,
            )
            .expect("write failing persona workflow");
            let (log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::new(2, RetryPolicy::Linear { delay_ms: 0 }),
                crate::AutonomyTier::ActAuto,
                crate::orchestration::CapabilityPolicy::default(),
                None,
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "persona-failure"))
                .await
                .expect("failed persona dispatch is recorded");

            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert_eq!(outcomes[0].attempt_count, 2);
            assert_eq!(outcomes[0].error.as_deref(), Some("persona failed"));

            let lifecycle = read_topic(log, crate::personas::PERSONA_RUNTIME_TOPIC).await;
            let kinds = lifecycle
                .iter()
                .map(|(_, event)| event.kind.as_str())
                .collect::<Vec<_>>();
            let released = kinds
                .iter()
                .position(|kind| *kind == "persona.lease.released")
                .expect("lease released");
            let failed = kinds
                .iter()
                .position(|kind| *kind == "persona.run.failed")
                .expect("run failed");
            assert!(released < failed);
            assert_eq!(
                kinds
                    .iter()
                    .filter(|kind| **kind == "persona.run.started")
                    .count(),
                2
            );
            assert_eq!(
                kinds
                    .iter()
                    .filter(|kind| **kind == "persona.run.failed")
                    .count(),
                2
            );
            assert_eq!(
                kinds
                    .iter()
                    .filter(|kind| **kind == "persona.lease.released")
                    .count(),
                2
            );
            assert!(!kinds.contains(&"persona.run.completed"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn persona_dispatch_cancellation_records_one_failed_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("merge_captain.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

pub pipeline run(_event) {
  while !is_cancelled() {
    sleep(1)
  }
  return "cancelled"
}
"#,
            )
            .expect("write cancellable persona workflow");
            let (log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
                crate::AutonomyTier::ActAuto,
                crate::orchestration::CapabilityPolicy::default(),
                None,
            )
            .await;
            let binding =
                resolve_live_trigger_binding("persona.merge_captain.github.issues.opened", None)
                    .expect("resolve persona trigger binding");
            let event = trigger_event("issues.opened", "persona-cancel");
            let event_id = event.id.0.clone();
            let binding_key = binding.binding_key();

            let run_dispatcher = dispatcher.clone();
            let handle = tokio::task::spawn_local(async move {
                run_dispatcher
                    .dispatch(&binding, event)
                    .await
                    .expect("cancelled persona dispatch is recorded")
            });

            wait_for_dispatcher_in_flight(&dispatcher, 1).await;
            append_dispatch_cancel_request(
                &log,
                &DispatchCancelRequest {
                    binding_key,
                    event_id,
                    requested_at: test_cancel_requested_at(),
                    requested_by: Some("test".to_string()),
                    audit_id: Some("persona-cancel-audit".to_string()),
                },
            )
            .await
            .expect("append persona cancel request");

            let outcome = handle.await.expect("join persona dispatch");
            assert_eq!(outcome.status, DispatchStatus::Cancelled);

            let lifecycle = read_topic(log, crate::personas::PERSONA_RUNTIME_TOPIC).await;
            let count = |kind: &str| {
                lifecycle
                    .iter()
                    .filter(|(_, event)| event.kind == kind)
                    .count()
            };
            assert_eq!(count("persona.run.started"), 1);
            assert_eq!(count("persona.run.failed"), 1);
            assert_eq!(count("persona.lease.released"), 1);
            assert_eq!(count("persona.run.completed"), 0);
        })
        .await;
}
