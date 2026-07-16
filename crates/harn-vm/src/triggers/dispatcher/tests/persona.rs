use std::path::Path;

use super::*;

async fn persona_dispatcher(
    module_path: &Path,
    function_name: &str,
    retry: TriggerRetryConfig,
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
            callable: crate::value::VmCallable::Lazy(crate::value::LazyVmCallable::new(
                module_path.to_path_buf(),
                function_name,
            )),
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
        max_autonomous_decisions_per_day: None,
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

pub fn run(event: TriggerEvent) -> dict {
  return {executed: true, kind: event.kind}
}
"#,
            )
            .expect("write persona workflow");
            let (log, dispatcher) =
                persona_dispatcher(&module_path, "run", TriggerRetryConfig::default()).await;

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

pub fn run(_event: TriggerEvent) -> dict {
  throw "persona failed"
}
"#,
            )
            .expect("write failing persona workflow");
            let (log, dispatcher) = persona_dispatcher(
                &module_path,
                "run",
                TriggerRetryConfig::new(1, RetryPolicy::Linear { delay_ms: 0 }),
            )
            .await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "persona-failure"))
                .await
                .expect("failed persona dispatch is recorded");

            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Dlq);
            assert_eq!(outcomes[0].attempt_count, 1);
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
            assert!(!kinds.contains(&"persona.run.completed"));
        })
        .await;
}
