use std::rc::Rc;

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
    vm.register_async_builtin("with_autonomy_policy", |args| async move {
        let policy_value = args
            .first()
            .ok_or_else(|| VmError::Runtime("with_autonomy_policy: policy is required".into()))?;
        let policy = serde_json::from_value::<crate::autonomy::AutonomyPolicy>(
            crate::llm::helpers::vm_value_to_json(policy_value),
        )
        .map_err(|error| {
            VmError::Runtime(format!("with_autonomy_policy: invalid policy: {error}"))
        })?;
        let closure = closure_arg(&args, 1, "with_autonomy_policy")?;
        let _guard = crate::autonomy::push_autonomy_policy(policy);
        call_scoped_closure(closure, "with_autonomy_policy").await
    });

    vm.register_async_builtin("with_execution_policy", |args| async move {
        let policy_value = args
            .first()
            .ok_or_else(|| VmError::Runtime("with_execution_policy: policy is required".into()))?;
        let policy = serde_json::from_value::<crate::orchestration::CapabilityPolicy>(
            crate::llm::helpers::vm_value_to_json(policy_value),
        )
        .map_err(|error| {
            VmError::Runtime(format!("with_execution_policy: invalid policy: {error}"))
        })?;
        let closure = closure_arg(&args, 1, "with_execution_policy")?;
        crate::orchestration::push_execution_policy(policy);
        let _guard = ScopeGuard::new(crate::orchestration::pop_execution_policy);
        call_scoped_closure(closure, "with_execution_policy").await
    });

    vm.register_async_builtin("with_approval_policy", |args| async move {
        let policy_value = args
            .first()
            .ok_or_else(|| VmError::Runtime("with_approval_policy: policy is required".into()))?;
        let policy = serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
            crate::llm::helpers::vm_value_to_json(policy_value),
        )
        .map_err(|error| {
            VmError::Runtime(format!("with_approval_policy: invalid policy: {error}"))
        })?;
        let closure = closure_arg(&args, 1, "with_approval_policy")?;
        crate::orchestration::push_approval_policy(policy);
        let _guard = ScopeGuard::new(crate::orchestration::pop_approval_policy);
        call_scoped_closure(closure, "with_approval_policy").await
    });

    vm.register_async_builtin("with_command_policy", |args| async move {
        let policy =
            crate::orchestration::parse_command_policy_value(args.first(), "with_command_policy")?
                .ok_or_else(|| {
                    VmError::Runtime("with_command_policy: policy is required".into())
                })?;
        let closure = closure_arg(&args, 1, "with_command_policy")?;
        crate::orchestration::push_command_policy(policy);
        let _guard = ScopeGuard::new(crate::orchestration::pop_command_policy);
        call_scoped_closure(closure, "with_command_policy").await
    });

    vm.register_async_builtin("with_dynamic_permissions", |args| async move {
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
    });
}
