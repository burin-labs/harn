use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_command_policy_builtins(vm: &mut Vm) {
    vm.register_builtin("command_policy", |args, _out| {
        let config = args.first().ok_or_else(|| {
            VmError::Runtime("command_policy: config dict is required".to_string())
        })?;
        crate::orchestration::normalize_command_policy_value(config)
    });

    vm.register_builtin("command_policy_push", |args, _out| {
        let policy =
            crate::orchestration::parse_command_policy_value(args.first(), "command_policy_push")?
                .ok_or_else(|| {
                    VmError::Runtime("command_policy_push: policy is required".to_string())
                })?;
        crate::orchestration::push_command_policy(policy);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("command_policy_pop", |_args, _out| {
        crate::orchestration::pop_command_policy();
        Ok(VmValue::Nil)
    });

    vm.register_builtin("command_risk_scan", |args, _out| {
        let ctx = args
            .first()
            .ok_or_else(|| VmError::Runtime("command_risk_scan: ctx is required".to_string()))?;
        crate::orchestration::command_risk_scan_value(ctx)
    });

    vm.register_builtin("command_result_scan", |args, _out| {
        let ctx = args
            .first()
            .ok_or_else(|| VmError::Runtime("command_result_scan: ctx is required".to_string()))?;
        crate::orchestration::command_result_scan_value(ctx)
    });

    vm.register_builtin("command_llm_risk_scan", |args, _out| {
        let ctx = args.first().ok_or_else(|| {
            VmError::Runtime("command_llm_risk_scan: ctx is required".to_string())
        })?;
        crate::orchestration::command_llm_risk_scan_value(ctx, args.get(1))
    });
}
