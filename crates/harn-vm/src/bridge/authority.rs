//! Capability injection for in-process host exports.
//!
//! The JSON bridge intentionally cannot serialize authority-bearing Harness
//! values. Host exports declare their required nominal handle as the leading
//! parameter, and this boundary derives that handle from the child VM's root.

use harn_builtin_meta::CapabilityId;
use harn_parser::TypeExpr;

use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;

pub(super) fn reminder_role_hint(
    value: Option<&str>,
) -> Result<crate::llm::helpers::ReminderRoleHint, &'static str> {
    use crate::llm::helpers::ReminderRoleHint;

    match value {
        None | Some("system") => Ok(ReminderRoleHint::System),
        Some("developer") => Ok(ReminderRoleHint::Developer),
        Some("user_block") => Ok(ReminderRoleHint::UserBlock),
        Some("ephemeral_cache") => Ok(ReminderRoleHint::EphemeralCache),
        Some(_) => {
            Err("`role_hint` must be one of system, developer, user_block, or ephemeral_cache")
        }
    }
}

pub(super) fn directive_authority(
    value: Option<&str>,
) -> Result<crate::llm::helpers::DirectiveAuthority, &'static str> {
    use crate::llm::helpers::DirectiveAuthority;

    match value {
        None | Some("contract") => Ok(DirectiveAuthority::Contract),
        Some("corrective") => Ok(DirectiveAuthority::Corrective),
        Some("advisory") => Ok(DirectiveAuthority::Advisory),
        Some(_) => Err("`authority` must be one of contract, corrective, or advisory"),
    }
}

/// Prepend the root or nominal sub-handle declared by a callable's leading
/// parameter. Pure callables with no authority parameter are unchanged.
pub fn inject_leading_authority(
    vm: &Vm,
    closure: &VmClosure,
    args: &[VmValue],
    callable_label: &str,
) -> Result<Vec<VmValue>, VmError> {
    let Some(type_name) = closure
        .func
        .params
        .first()
        .and_then(|param| param.type_expr.as_ref())
        .and_then(named_type)
    else {
        return Ok(args.to_vec());
    };

    let capability = CapabilityId::from_type_name(type_name);
    if type_name != "Harness" && capability.is_none() {
        return Ok(args.to_vec());
    }

    let authority = if type_name == "Harness" {
        vm.root_harness_value()
    } else {
        capability.and_then(|capability| {
            let VmValue::Harness(root) = vm.root_harness_value()? else {
                return None;
            };
            root.sub_handle(capability.field_name())
                .map(VmValue::harness)
        })
    }
    .ok_or_else(|| {
        VmError::Runtime(format!(
            "{callable_label} requires `{type_name}`, \
             but no root Harness is installed"
        ))
    })?;

    let mut call_args = Vec::with_capacity(args.len() + 1);
    call_args.push(authority);
    call_args.extend_from_slice(args);
    Ok(call_args)
}

fn named_type(type_expr: &TypeExpr) -> Option<&str> {
    match type_expr {
        TypeExpr::Named(name) => Some(name),
        _ => None,
    }
}
