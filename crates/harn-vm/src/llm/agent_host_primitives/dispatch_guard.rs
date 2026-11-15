use crate::value::{VmError, VmValue};

/// A caller-owned guard is checked after approval and interception, immediately
/// before handing the invocation to its executor. Only explicit true permits it.
pub(super) async fn permits(
    ctx: &crate::vm::AsyncBuiltinCtx,
    guard: Option<&VmValue>,
    arguments: &serde_json::Value,
) -> Result<bool, VmError> {
    match guard {
        None | Some(VmValue::Nil) => Ok(true),
        Some(VmValue::Closure(guard)) => {
            let arguments = crate::schema::json_to_vm_value(arguments);
            let value = ctx.child_vm().call_closure_pub(guard, &[arguments]).await?;
            Ok(matches!(value, VmValue::Bool(true)))
        }
        Some(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeBuiltinCtxExt;

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_requires_an_explicit_guard_grant_before_reaching_the_executor() {
        use super::super::tool_failure_recording_tests::compiled_closure;
        let handler = compiled_closure("handler", "fn handler(args: dict) { return \"executed\" }");
        let tools = VmValue::dict([(
            "tools",
            VmValue::list(vec![VmValue::dict([
                ("name", VmValue::string("guarded")),
                ("handler", VmValue::Closure(handler)),
            ])]),
        )]);
        for (source, granted) in [("true", true), ("false", false), ("nil", false)] {
            let guard = compiled_closure(
                "may_execute",
                &format!("fn may_execute(arguments: dict) {{ return {source} }}"),
            );
            let call = VmValue::dict([
                ("id", VmValue::string(format!("guard-{source}"))),
                ("name", VmValue::string("guarded")),
                ("arguments", VmValue::dict([] as [(&str, VmValue); 0])),
                ("execution_guard", VmValue::Closure(guard)),
            ]);
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            let result = super::super::host_agent_dispatch_tool_call(
                crate::vm::AsyncBuiltinCtx::for_test(vm),
                call,
                Some(&tools),
                &crate::value::DictMap::new(),
            )
            .await
            .unwrap();
            let result = crate::llm::helpers::vm_value_to_json(&result);
            assert_eq!(result["ok"], granted, "{result}");
            if granted {
                assert_eq!(result["rendered_result"], "executed");
            } else {
                assert_eq!(result["error_category"], "permission_denied");
                assert_eq!(
                    result["denial"]["machine_reason"],
                    "execution_contract_changed"
                );
                assert_ne!(result["rendered_result"], "executed");
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guard_checks_the_arguments_after_interception() {
        use super::super::tool_failure_recording_tests::compiled_closure;
        use crate::orchestration::ToolInterceptionScope;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let interception = ToolInterceptionScope::new();
        for replacement in ["approved", "rewritten"] {
            let hooks = Arc::new(AtomicUsize::new(0));
            let observed = hooks.clone();
            interception.before("guarded", move || {
                observed.fetch_add(1, Ordering::SeqCst);
                Some(serde_json::json!({"resource": replacement}))
            });
            let handler =
                compiled_closure("handler", "fn handler(args: dict) { return args.resource }");
            let guard = compiled_closure(
                "may_execute",
                "fn may_execute(arguments: dict) { return arguments.resource == \"approved\" }",
            );
            let tools = VmValue::dict([(
                "tools",
                VmValue::list(vec![VmValue::dict([
                    ("name", VmValue::string("guarded")),
                    ("handler", VmValue::Closure(handler)),
                ])]),
            )]);
            let call = VmValue::dict([
                ("id", VmValue::string(format!("guard-{replacement}"))),
                ("name", VmValue::string("guarded")),
                (
                    "arguments",
                    VmValue::dict([("resource", VmValue::string("approved"))]),
                ),
                ("execution_guard", VmValue::Closure(guard)),
            ]);
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            let result = super::super::host_agent_dispatch_tool_call(
                crate::vm::AsyncBuiltinCtx::for_test(vm),
                call,
                Some(&tools),
                &crate::value::DictMap::new(),
            )
            .await
            .unwrap();
            let result = crate::llm::helpers::vm_value_to_json(&result);
            assert_eq!(hooks.load(Ordering::SeqCst), 1, "{result}");
            assert_eq!(result["ok"], replacement == "approved", "{result}");
            if replacement == "approved" {
                assert_eq!(result["rendered_result"], "approved");
            } else {
                assert_eq!(
                    result["denial"]["machine_reason"],
                    "execution_contract_changed"
                );
                assert_ne!(result["rendered_result"], "rewritten");
            }
            interception.clear_hooks();
        }
    }
}
