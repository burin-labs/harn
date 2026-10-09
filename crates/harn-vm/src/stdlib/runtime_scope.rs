use std::sync::Arc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

fn closure_arg(args: &[VmValue], index: usize, label: &str) -> Result<Arc<VmClosure>, VmError> {
    match args.get(index) {
        Some(VmValue::Closure(closure)) => Ok(closure.clone()),
        _ => Err(VmError::Runtime(format!(
            "{label}: second argument must be a closure"
        ))),
    }
}

async fn call_scoped_closure(
    ctx: &AsyncBuiltinCtx,
    closure: Arc<VmClosure>,
    _label: &str,
) -> Result<VmValue, VmError> {
    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&closure, &[]).await;
    let output = child_vm.take_output();
    ctx.forward_output(&output);
    result
}

pub(crate) fn register_runtime_scope_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    exposure = "harness.runtime.evaluate_approval_policy",
    effects = [],
    sig = "evaluate_approval_policy(policy: dict, request: @TOOL_APPROVAL_REQUEST) -> @APPROVAL_POLICY_DECISION",
    category = "runtime_scope"
)]
fn evaluate_approval_policy_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("evaluate_approval_policy: policy is required".into()))?;
    let policy = crate::orchestration::ToolApprovalPolicy::from_host_json(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| {
        VmError::Runtime(format!("evaluate_approval_policy: invalid policy: {error}"))
    })?;
    let request_value = args
        .get(1)
        .ok_or_else(|| VmError::Runtime("evaluate_approval_policy: request is required".into()))?;
    let request = crate::orchestration::ToolApprovalRequest::from_host_json(
        crate::llm::helpers::vm_value_to_json(request_value),
    )
    .map_err(|error| {
        VmError::Runtime(format!(
            "evaluate_approval_policy: invalid request: {error}"
        ))
    })?;
    let decision = policy.evaluate_request(&request);
    let json = serde_json::to_value(decision).map_err(|error| {
        VmError::Runtime(format!(
            "evaluate_approval_policy: serialize decision failed: {error}"
        ))
    })?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[harn_builtin(
    exposure = "harness.runtime.current_policy",
    effects = ["state.read@const=runtime-policy"],
    sig = "current_policy() -> any", category = "runtime_scope"
)]
fn current_policy_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(VmValue::Nil);
    };
    let json = serde_json::to_value(policy).map_err(|error| {
        VmError::Runtime(format!("current_policy: serialize policy failed: {error}"))
    })?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[harn_builtin(
    exposure = "harness.runtime.with_autonomy_policy",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_autonomy_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_autonomy_policy_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("with_autonomy_policy: policy is required".into()))?;
    let policy = serde_json::from_value::<crate::autonomy::AutonomyPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| VmError::Runtime(format!("with_autonomy_policy: invalid policy: {error}")))?;
    let closure = closure_arg(&args, 1, "with_autonomy_policy")?;
    crate::orchestration::scope_autonomy_policy(
        policy,
        call_scoped_closure(&ctx, closure, "with_autonomy_policy"),
    )
    .await
}

#[harn_builtin(
    exposure = "harness.runtime.with_execution_policy",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_execution_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_execution_policy_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("with_execution_policy: policy is required".into()))?;
    let requested = serde_json::from_value::<crate::orchestration::CapabilityPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| VmError::Runtime(format!("with_execution_policy: invalid policy: {error}")))?;
    let closure = closure_arg(&args, 1, "with_execution_policy")?;
    let effective = match crate::orchestration::current_execution_policy() {
        Some(outer) => outer.intersect(&requested).map_err(VmError::Runtime)?,
        None => requested,
    };
    crate::orchestration::scope_execution_policy(
        effective,
        call_scoped_closure(&ctx, closure, "with_execution_policy"),
    )
    .await
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__harn_with_execution_policy_override(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope",
    runtime_only = true
)]
async fn harn_with_execution_policy_override_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let policy_value = args.first().ok_or_else(|| {
        VmError::Runtime("__harn_with_execution_policy_override: policy is required".into())
    })?;
    let policy = serde_json::from_value::<crate::orchestration::CapabilityPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| {
        VmError::Runtime(format!(
            "__harn_with_execution_policy_override: invalid policy: {error}"
        ))
    })?;
    let closure = closure_arg(&args, 1, "__harn_with_execution_policy_override")?;
    crate::orchestration::scope_execution_policy(
        policy,
        call_scoped_closure(&ctx, closure, "__harn_with_execution_policy_override"),
    )
    .await
}

#[harn_builtin(
    exposure = "harness.runtime.with_approval_policy",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_approval_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_approval_policy_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("with_approval_policy: policy is required".into()))?;
    let policy = serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| VmError::Runtime(format!("with_approval_policy: invalid policy: {error}")))?;
    let closure = closure_arg(&args, 1, "with_approval_policy")?;
    crate::orchestration::scope_approval_policy(
        policy,
        call_scoped_closure(&ctx, closure, "with_approval_policy"),
    )
    .await
}

#[harn_builtin(
    exposure = "harness.runtime.with_command_policy",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_command_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_command_policy_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let policy =
        crate::orchestration::parse_command_policy_value(args.first(), "with_command_policy")?
            .ok_or_else(|| VmError::Runtime("with_command_policy: policy is required".into()))?;
    let closure = closure_arg(&args, 1, "with_command_policy")?;
    crate::orchestration::scope_command_policy(
        policy,
        call_scoped_closure(&ctx, closure, "with_command_policy"),
    )
    .await
}

#[harn_builtin(
    exposure = "harness.runtime.with_dynamic_permissions",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_dynamic_permissions(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_dynamic_permissions_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let permissions = crate::llm::permissions::parse_dynamic_permission_policy(
        args.first(),
        "with_dynamic_permissions",
    )?
    .ok_or_else(|| {
        VmError::Runtime("with_dynamic_permissions: permissions policy is required".into())
    })?;
    let closure = closure_arg(&args, 1, "with_dynamic_permissions")?;
    crate::orchestration::scope_dynamic_permissions(
        permissions,
        call_scoped_closure(&ctx, closure, "with_dynamic_permissions"),
    )
    .await
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &EVALUATE_APPROVAL_POLICY_IMPL_DEF,
    &CURRENT_POLICY_IMPL_DEF,
    &WITH_AUTONOMY_POLICY_IMPL_DEF,
    &WITH_EXECUTION_POLICY_IMPL_DEF,
    &HARN_WITH_EXECUTION_POLICY_OVERRIDE_IMPL_DEF,
    &WITH_APPROVAL_POLICY_IMPL_DEF,
    &WITH_COMMAND_POLICY_IMPL_DEF,
    &WITH_DYNAMIC_PERMISSIONS_IMPL_DEF,
];

#[cfg(test)]
mod approval_decision_tests {
    use super::*;
    use serde_json::{json, Value};

    fn evaluate(policy: Value, request: Value) -> Result<Value, VmError> {
        let args = [
            crate::stdlib::json_to_vm_value(&policy),
            crate::stdlib::json_to_vm_value(&request),
        ];
        evaluate_approval_policy_impl(&args, &mut String::new())
            .map(|value| crate::llm::helpers::vm_value_to_json(&value))
    }

    #[test]
    fn host_entry_point_preserves_canonical_decisions_and_receipts() {
        let workspace = tempfile::tempdir().unwrap();
        for (policy, request, expected) in [
            (
                json!({}),
                json!({"tool_name":"run", "arguments":{}}),
                "allow",
            ),
            (
                json!({"auto_deny":["run"]}),
                json!({"tool_name":"run", "arguments":{}}),
                "deny",
            ),
            (
                json!({"require_approval":["run"]}),
                json!({"tool_name":"run", "arguments":{}}),
                "ask",
            ),
            (
                json!({}),
                json!({"tool_name":"read_file", "arguments":{"path":".env"},
                    "workspace_boundary":{"root":workspace.path()}}),
                "deny",
            ),
            (
                json!({"rules":[{"id":"deny-command", "action":"deny", "match":{"command":"danger"}}]}),
                json!({"tool_name":"run", "arguments":{"command":"danger"}, "policy_decision":{"context":{"rawInput":{"command":"danger"}}}}),
                "deny",
            ),
        ] {
            let actual = evaluate(policy.clone(), request.clone()).expect("valid explicit input");
            let canonical = crate::orchestration::ToolApprovalPolicy::from_host_json(policy)
                .unwrap()
                .evaluate_request(
                    &crate::orchestration::ToolApprovalRequest::from_host_json(request).unwrap(),
                );
            assert_eq!(
                actual["action"], expected,
                "known allow, ask and deny controls must differ"
            );
            assert_eq!(actual, serde_json::to_value(canonical).unwrap());
            assert_eq!(
                actual["receipt"]["type"],
                "harn.permission_policy_decision.v1"
            );
        }
    }

    #[test]
    fn malformed_host_input_never_defaults_to_allow() {
        let request = json!({"tool_name":"run", "arguments":{}});
        for policy in [
            json!(null),
            json!([]),
            json!({"auto_denyy":["run"]}),
            json!({"rules":"deny"}),
        ] {
            assert!(evaluate(policy, request.clone()).is_err());
        }
        for request in [
            json!([]),
            json!(["run", {}]),
            json!({}),
            json!({"tool_name":"", "arguments":{}}),
            json!({"tool_name":"run", "arguments":null}),
            json!({"tool_name":"run", "arguments":{}, "repeat_count":-1}),
            json!({"tool_name":"run", "arguments":{}, "policy_decison":{}}),
            json!({"tool_name":"run", "arguments":{}, "policy_decision":true}),
            json!({"tool_name":"read_file", "arguments":{"path":"../outside.txt"}}),
            json!({"tool_name":"read_file", "arguments":{"path":null}}),
        ] {
            assert!(evaluate(json!({}), request).is_err());
        }
    }
}
