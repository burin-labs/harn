use crate::value::{VmError, VmValue};

pub(super) fn refusal() -> crate::agent_events::ToolDenial {
    crate::agent_events::ToolDenial::terminal(
        crate::agent_events::DenialGate::DeterministicPrecheck,
        None,
        "Execution contract changed while this tool call waited; execution refused.",
    )
    .with_audiences(Some("execution_contract_changed".into()), None)
}

/// A caller-owned guard is checked after approval and interception, immediately
/// before handing the invocation to its executor. Only explicit true permits it.
pub(super) async fn permits(
    ctx: &crate::vm::AsyncBuiltinCtx,
    guard: Option<&VmValue>,
) -> Result<bool, VmError> {
    match guard {
        None | Some(VmValue::Nil) => Ok(true),
        Some(VmValue::Closure(guard)) => {
            let value = ctx.child_vm().call_closure_pub(guard, &[]).await?;
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
            let guard = compiled_closure("guard", &format!("fn guard() {{ return {source} }}"));
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
}
