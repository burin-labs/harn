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

/// Backward-compatible name for [`inject_leading_authorities`].
pub fn inject_leading_authority(
    vm: &Vm,
    closure: &VmClosure,
    args: &[VmValue],
    callable_label: &str,
) -> Result<Vec<VmValue>, VmError> {
    inject_leading_authorities(vm, closure, args, callable_label)
}

/// Prepend every root or nominal Harness handle in a callable's contiguous
/// authority prefix. Pure callables with no authority prefix are unchanged.
///
/// Capability migration can legitimately widen a host export from one handle
/// to several (for example `HarnessPostgres, HarnessProcess, args`). The host
/// bridge owns that prefix and derives each handle from the same installed root
/// Harness; JSON arguments begin at the first non-authority parameter.
pub fn inject_leading_authorities(
    vm: &Vm,
    closure: &VmClosure,
    args: &[VmValue],
    callable_label: &str,
) -> Result<Vec<VmValue>, VmError> {
    let mut call_args = Vec::with_capacity(closure.func.params.len().max(args.len()));
    for param in &closure.func.params {
        let type_name = authority_type_name(&param.name, param.type_expr.as_ref());
        let Some(type_name) = type_name else {
            break;
        };

        let capability = CapabilityId::from_type_name(type_name);
        if type_name != "Harness" && capability.is_none() {
            break;
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
        call_args.push(authority);
    }

    call_args.extend_from_slice(args);
    Ok(call_args)
}

/// Count the contiguous host-injected authority prefix in a callable signature.
///
/// JSON-facing adapters use this to project only caller-owned domain inputs.
/// Keeping the classification beside authority injection prevents an adapter
/// from advertising an unforgeable `Harness*` value as ordinary JSON.
pub fn leading_authority_param_count(params: &[harn_parser::TypedParam]) -> usize {
    params
        .iter()
        .take_while(|param| authority_type_name(&param.name, param.type_expr.as_ref()).is_some())
        .count()
}

fn authority_type_name<'a>(name: &'a str, type_expr: Option<&'a TypeExpr>) -> Option<&'a str> {
    // Preserve the long-standing untyped `harness` entrypoint convention while
    // making explicit nominal Harness types independent of the local name.
    let type_name = type_expr
        .and_then(named_type)
        .or_else(|| (name == "harness").then_some("Harness"))?;
    (type_name == "Harness" || CapabilityId::from_type_name(type_name).is_some())
        .then_some(type_name)
}

pub(super) fn inject_export_authority(
    vm: &Vm,
    closure: &VmClosure,
    args: &[VmValue],
    export_name: &str,
) -> Result<Vec<VmValue>, VmError> {
    inject_leading_authorities(
        vm,
        closure,
        args,
        &format!("playground host export `{export_name}`"),
    )
}

fn named_type(type_expr: &TypeExpr) -> Option<&str> {
    match type_expr {
        TypeExpr::Named(name) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn injects_every_leading_authority_and_calls_the_export() {
        let mut vm = Vm::new();
        crate::stdlib::register_vm_stdlib(&mut vm);
        vm.set_harness(crate::Harness::real());
        let exports = vm
            .load_module_exports_from_source(
                "<multi-authority-host-export>",
                "pub fn dispatch(postgres: HarnessPostgres, process: HarnessProcess, args: dict) -> dict { return args }",
            )
            .await
            .expect("load multi-authority export");
        let closure = exports.get("dispatch").expect("dispatch export");
        let input = VmValue::dict(crate::value::DictMap::default());

        let call_args = inject_leading_authorities(&vm, closure, &[input], "dispatch")
            .expect("inject authority prefix");
        assert_eq!(call_args.len(), 3);
        assert!(
            matches!(&call_args[0], VmValue::Harness(handle) if handle.type_name() == "HarnessPostgres")
        );
        assert!(
            matches!(&call_args[1], VmValue::Harness(handle) if handle.type_name() == "HarnessProcess")
        );

        vm.call_closure_pub(closure, &call_args)
            .await
            .expect("multi-authority export accepts injected prefix");
    }

    #[test]
    fn counts_only_the_contiguous_authority_prefix() {
        let program = harn_parser::parse_source(
            "pub fn dispatch(root: Harness, process: HarnessProcess, args: dict, later: HarnessFs) {}",
        )
        .expect("parse fixture");
        let (_, node) = harn_parser::peel_attributes(&program[0]);
        let harn_parser::Node::FnDecl { params, .. } = &node.node else {
            panic!("expected function")
        };
        assert_eq!(leading_authority_param_count(params), 2);
    }
}
