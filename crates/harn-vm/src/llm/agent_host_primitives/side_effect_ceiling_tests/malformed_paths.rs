//! Model path-shape mistakes are retryable before policy callbacks or prompts.

use super::*;
use crate::value::{VmDictExt, VmValue};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Calls {
    precheck: Arc<AtomicUsize>,
    effect: Arc<AtomicUsize>,
}

impl Calls {
    fn new() -> Self {
        Self {
            precheck: Arc::new(AtomicUsize::new(0)),
            effect: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn dispatch(
        &self,
        arguments: serde_json::Value,
        annotated: bool,
        options: &crate::value::DictMap,
    ) -> serde_json::Value {
        let handler = super::super::tool_failure_recording_tests::compiled_closure(
            "handler",
            "fn handler(request: dict) { return len([1]) }",
        );
        let mut entry = crate::value::DictMap::new();
        entry.put("name", VmValue::string("read_file"));
        entry.put("handler", VmValue::Closure(handler));
        if annotated {
            entry.put(
                "annotations",
                json_to_vm_value(&serde_json::json!({
                    "kind": "execute", "side_effect_level": "process_exec",
                    "arg_schema": {"path_params": ["location"]}
                })),
            );
        }
        let catalog = VmValue::dict([(
            "tools",
            VmValue::List(Arc::new(vec![VmValue::dict_map(entry)])),
        )]);
        let mut vm = crate::vm::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let precheck = self.precheck.clone();
        let effect = self.effect.clone();
        // Both counters run through actual compiled closures. The corrected
        // control below must reach both, establishing that zero is measured.
        vm.register_builtin("len", move |args, _| {
            let Some(VmValue::List(items)) = args.first() else {
                panic!("fixture len receives a list")
            };
            match items.len() {
                2 => {
                    precheck.fetch_add(1, Ordering::SeqCst);
                }
                1 => {
                    effect.fetch_add(1, Ordering::SeqCst);
                }
                _ => panic!("unexpected fixture counter"),
            }
            Ok(VmValue::Int(items.len() as i64))
        });
        let call = json_to_vm_value(&serde_json::json!({
            "id": "path-shape-call", "name": "read_file", "arguments": arguments
        }));
        let result = host_agent_dispatch_tool_call(
            crate::vm::AsyncBuiltinCtx::for_test(vm),
            call,
            Some(&catalog),
            options,
        )
        .await
        .expect("malformed model input returns normal feedback");
        crate::llm::helpers::vm_value_to_json(&result)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn catalog_paths_reach_workspace_approval_before_dispatch() {
    clear_execution_policy_stacks();
    clear_all_approval_policy_repeat_counts();
    let _bridge = HostBridgeGuard::replace(None);
    push_approval_policy(ToolApprovalPolicy {
        auto_approve: vec!["read_file".into()],
        ..ToolApprovalPolicy::default()
    });
    let calls = Calls::new();
    let outside = std::env::temp_dir().join("harn-catalog-path-approval-proof");
    let refused = calls
        .dispatch(
            serde_json::json!({"location": outside}),
            true,
            &crate::value::DictMap::new(),
        )
        .await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(calls.effect.load(Ordering::SeqCst), 0);

    let allowed = calls
        .dispatch(
            serde_json::json!({"location": "proof"}),
            true,
            &crate::value::DictMap::new(),
        )
        .await;
    assert_eq!(allowed["ok"], true, "{allowed}");
    assert_eq!(calls.effect.load(Ordering::SeqCst), 1);
    pop_approval_policy();
    clear_all_approval_policy_repeat_counts();
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_paths_retry_before_policy_callbacks_approval_and_effects() {
    clear_execution_policy_stacks();
    crate::orchestration::clear_tool_prechecks();
    clear_all_approval_policy_repeat_counts();
    crate::orchestration::push_tool_precheck(
        super::super::tool_failure_recording_tests::compiled_closure(
            "precheck",
            "fn precheck(request: dict) { const counted = len([1, 2]); return nil }",
        ),
    );
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let _bridge = HostBridgeGuard::replace(Some(responding_bridge(
        crate::llm::acp_permission::allow_response(),
        captured.clone(),
    )));
    for annotation_source in ["conventional", "catalog", "options"] {
        let annotated = annotation_source == "catalog";
        let field = if annotation_source == "conventional" {
            "path"
        } else {
            "location"
        };
        let options = match annotation_source {
            "conventional" => crate::value::DictMap::new(),
            "catalog" => policy_options_without_annotations("malformed-declared-path"),
            "options" => {
                let mut options = policy_options_without_annotations("malformed-option-path");
                options.put(
                    "policy",
                    json_to_vm_value(&serde_json::json!({
                        "tools": ["read_file"], "side_effect_level": "read_only",
                        "tool_annotations": {"read_file": {
                            "kind": "execute", "side_effect_level": "process_exec",
                            "arg_schema": {"path_params": ["location"]}
                        }}
                    })),
                );
                options
            }
            _ => unreachable!(),
        };
        push_approval_policy(
            serde_json::from_value(serde_json::json!({
                "rules": [{"ask": {"tool": "read_file"}, "reason": "fixture asks"}]
            }))
            .expect("ask policy"),
        );
        let calls = Calls::new();
        let initial_prompts = captured.lock().unwrap().len();
        for malformed in [serde_json::json!(42), serde_json::json!(["proof", 42])] {
            let result = calls
                .dispatch(serde_json::json!({(field): malformed}), annotated, &options)
                .await;
            assert_eq!(result["error_category"], "schema_validation", "{result}");
            assert!(result["denial"].is_null(), "{result}");
            assert!(result["result"].get("denial").is_none(), "{result}");
            assert_eq!(result["mutation_status"], "not_applied", "{result}");
            assert_eq!(calls.precheck.load(Ordering::SeqCst), 0);
            assert_eq!(calls.effect.load(Ordering::SeqCst), 0);
            assert_eq!(captured.lock().unwrap().len(), initial_prompts);
        }
        let corrected = calls
            .dispatch(serde_json::json!({(field): "proof"}), annotated, &options)
            .await;
        assert_eq!(corrected["ok"], true, "{corrected}");
        assert_eq!(calls.precheck.load(Ordering::SeqCst), 1);
        assert_eq!(calls.effect.load(Ordering::SeqCst), 1);
        assert!(
            captured.lock().unwrap().len() > initial_prompts,
            "corrected call requests approval"
        );
        pop_approval_policy();
    }
    push_approval_policy(
        serde_json::from_value(serde_json::json!({
            "rules": [{"deny": {"tool": "read_file"}, "reason": "fixture forbids reads"}]
        }))
        .expect("deny policy"),
    );
    let calls = Calls::new();
    let initial_prompts = captured.lock().unwrap().len();
    let denied = calls
        .dispatch(
            serde_json::json!({"path": "proof"}),
            false,
            &crate::value::DictMap::new(),
        )
        .await;
    pop_approval_policy();
    crate::orchestration::clear_tool_prechecks();
    clear_all_approval_policy_repeat_counts();
    assert_eq!(denied["error_category"], "permission_denied", "{denied}");
    assert!(
        !denied["denial"].is_null(),
        "true policy refusal retains ToolDenial: {denied}"
    );
    assert_eq!(calls.precheck.load(Ordering::SeqCst), 1);
    assert_eq!(calls.effect.load(Ordering::SeqCst), 0);
    assert_eq!(captured.lock().unwrap().len(), initial_prompts);
}
