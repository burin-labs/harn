use std::rc::Rc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

struct ScopeGuard {
    pop: fn(),
}

impl ScopeGuard {
    fn new(pop: fn()) -> Self {
        Self { pop }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        (self.pop)();
    }
}

fn closure_arg(args: &[VmValue], index: usize, label: &str) -> Result<Rc<VmClosure>, VmError> {
    match args.get(index) {
        Some(VmValue::Closure(closure)) => Ok(closure.clone()),
        _ => Err(VmError::Runtime(format!(
            "{label}: second argument must be a closure"
        ))),
    }
}

async fn call_scoped_closure(closure: Rc<VmClosure>, label: &str) -> Result<VmValue, VmError> {
    let mut child_vm = crate::vm::clone_async_builtin_child_vm()
        .ok_or_else(|| VmError::Runtime(format!("{label} requires an async VM context")))?;
    let result = child_vm.call_closure_pub(&closure, &[]).await;
    let output = child_vm.take_output();
    crate::vm::forward_child_output_to_parent(&output);
    result
}

pub(crate) fn register_runtime_scope_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "current_policy() -> any", category = "runtime_scope")]
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
    sig = "with_autonomy_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_autonomy_policy_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("with_autonomy_policy: policy is required".into()))?;
    let policy = serde_json::from_value::<crate::autonomy::AutonomyPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| VmError::Runtime(format!("with_autonomy_policy: invalid policy: {error}")))?;
    let closure = closure_arg(&args, 1, "with_autonomy_policy")?;
    let _guard = crate::autonomy::push_autonomy_policy(policy);
    call_scoped_closure(closure, "with_autonomy_policy").await
}

#[harn_builtin(
    sig = "with_execution_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_execution_policy_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    crate::orchestration::push_execution_policy(effective);
    let _guard = ScopeGuard::new(crate::orchestration::pop_execution_policy);
    call_scoped_closure(closure, "with_execution_policy").await
}

#[harn_builtin(
    sig = "__harn_with_execution_policy_override(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope",
    runtime_only = true
)]
async fn harn_with_execution_policy_override_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    crate::orchestration::push_execution_policy(policy);
    let _guard = ScopeGuard::new(crate::orchestration::pop_execution_policy);
    call_scoped_closure(closure, "__harn_with_execution_policy_override").await
}

#[harn_builtin(
    sig = "with_approval_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_approval_policy_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let policy_value = args
        .first()
        .ok_or_else(|| VmError::Runtime("with_approval_policy: policy is required".into()))?;
    let policy = serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
        crate::llm::helpers::vm_value_to_json(policy_value),
    )
    .map_err(|error| VmError::Runtime(format!("with_approval_policy: invalid policy: {error}")))?;
    let closure = closure_arg(&args, 1, "with_approval_policy")?;
    crate::orchestration::push_approval_policy(policy);
    let _guard = ScopeGuard::new(crate::orchestration::pop_approval_policy);
    call_scoped_closure(closure, "with_approval_policy").await
}

#[harn_builtin(
    sig = "with_command_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_command_policy_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let policy =
        crate::orchestration::parse_command_policy_value(args.first(), "with_command_policy")?
            .ok_or_else(|| VmError::Runtime("with_command_policy: policy is required".into()))?;
    let closure = closure_arg(&args, 1, "with_command_policy")?;
    crate::orchestration::push_command_policy(policy);
    let _guard = ScopeGuard::new(crate::orchestration::pop_command_policy);
    call_scoped_closure(closure, "with_command_policy").await
}

#[harn_builtin(
    sig = "with_dynamic_permissions(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_dynamic_permissions_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let permissions = crate::llm::permissions::parse_dynamic_permission_policy(
        args.first(),
        "with_dynamic_permissions",
    )?
    .ok_or_else(|| {
        VmError::Runtime("with_dynamic_permissions: permissions policy is required".into())
    })?;
    let closure = closure_arg(&args, 1, "with_dynamic_permissions")?;
    crate::llm::permissions::push_dynamic_permission_policy(permissions);
    let _guard = ScopeGuard::new(crate::llm::permissions::pop_dynamic_permission_policy);
    call_scoped_closure(closure, "with_dynamic_permissions").await
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &CURRENT_POLICY_IMPL_DEF,
    &WITH_AUTONOMY_POLICY_IMPL_DEF,
    &WITH_EXECUTION_POLICY_IMPL_DEF,
    &HARN_WITH_EXECUTION_POLICY_OVERRIDE_IMPL_DEF,
    &WITH_APPROVAL_POLICY_IMPL_DEF,
    &WITH_COMMAND_POLICY_IMPL_DEF,
    &WITH_DYNAMIC_PERMISSIONS_IMPL_DEF,
];
