use std::path::Path;

use super::*;
use crate::VmValue;

async fn lazy_dispatcher(
    module_path: &Path,
    autonomy_tier: crate::AutonomyTier,
    predicate: Option<&str>,
) -> Dispatcher {
    let lazy = |function_name: &str| {
        crate::value::VmCallable::Lazy(crate::value::LazyVmCallable::new(
            module_path.to_path_buf(),
            function_name,
        ))
    };
    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "lazy-github-issue".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier,
        handler: TriggerHandlerSpec::Local {
            raw: "local_fn".to_string(),
            callable: lazy("local_fn"),
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: predicate.map(|name| TriggerPredicateSpec {
            raw: name.to_string(),
            callable: lazy(name),
        }),
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
        definition_fingerprint: "lazy-handler-v1".to_string(),
    }])
    .await
    .expect("install lazy trigger");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.register_builtin("__test_current_policy", |_args, _out| {
        let level = crate::orchestration::current_execution_policy()
            .and_then(|policy| policy.side_effect_level)
            .unwrap_or_else(|| "none".to_string());
        Ok(VmValue::String(arcstr::ArcStr::from(level)))
    });
    vm.set_source_dir(module_path.parent().expect("module parent"));
    Dispatcher::with_event_log(vm, install_test_event_log())
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_module_initializes_under_context_and_shares_state_across_dispatches() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("lazy_handler.harn");
            std::fs::write(
                &module_path,
                r#"
import "std/triggers"

let initialized_context = handler_context()
let counter = 0

pub fn should_handle(event: TriggerEvent) -> bool {
  return event.kind == "issues.opened"
}

pub fn local_fn(_event: TriggerEvent) -> dict {
  counter = counter + 1
  return {count: counter, action: initialized_context.action}
}
"#,
            )
            .expect("write lazy handler module");
            let dispatcher = lazy_dispatcher(
                &module_path,
                crate::AutonomyTier::ActAuto,
                Some("should_handle"),
            )
            .await;

            for (delivery, expected_count) in [("delivery-lazy-1", 1), ("delivery-lazy-2", 2)] {
                let outcomes = dispatcher
                    .dispatch_event(trigger_event("issues.opened", delivery))
                    .await
                    .expect("lazy trigger dispatch succeeds");
                assert_eq!(outcomes.len(), 1);
                assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
                let result = outcomes[0].result.as_ref().expect("handler result");
                assert_eq!(result["count"], expected_count);
                assert!(result["action"].as_str().is_some());
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_initializer_observes_dispatch_capability_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("policy_handler.harn");
            std::fs::write(
                &module_path,
                r"
let initialized_policy = __test_current_policy()

pub fn local_fn(_event: any) -> string {
  return initialized_policy
}
",
            )
            .expect("write policy handler module");
            let dispatcher =
                lazy_dispatcher(&module_path, crate::AutonomyTier::Suggest, None).await;

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-policy"))
                .await
                .expect("policy observation dispatch succeeds");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Succeeded);
            assert_eq!(outcomes[0].result, Some(serde_json::json!("read_only")));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_initializer_observes_preexisting_dispatch_cancellation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::reset_thread_local_state();
            let dir = tempfile::tempdir().expect("tempdir");
            let module_path = dir.path().join("cancel_handler.harn");
            let marker = dir.path().join("cancel-marker.txt");
            std::fs::write(
                &module_path,
                r#"
write_file("cancel-marker.txt", "initialized")

pub fn local_fn(_event: any) -> string {
  return "handled"
}
"#,
            )
            .expect("write cancel handler module");
            let dispatcher =
                lazy_dispatcher(&module_path, crate::AutonomyTier::ActAuto, None).await;
            dispatcher.shutdown();

            let outcomes = dispatcher
                .dispatch_event(trigger_event("issues.opened", "delivery-cancel"))
                .await
                .expect("cancelled dispatch is recorded");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DispatchStatus::Cancelled);
            assert!(
                !marker.exists(),
                "initializer ran before cancellation was installed"
            );
        })
        .await;
}
