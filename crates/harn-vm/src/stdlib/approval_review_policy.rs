//! Runtime projection of the typed approval-review policy into Harn.

use crate::orchestration::ApprovalReviewPolicy;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&APPROVAL_REVIEW_DEFAULT_POLICY_BUILTIN_DEF];

pub(crate) fn register_approval_review_policy_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, MODULE_BUILTINS);
}

/// Project the one parsed policy owner into the Harn reviewer session.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__approval_review_default_policy() -> dict",
    category = "agent.policy"
)]
fn approval_review_default_policy_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let policy = serde_json::to_value(ApprovalReviewPolicy::bundled()).map_err(|error| {
        VmError::Runtime(format!("serialize bundled approval-review policy: {error}"))
    })?;
    Ok(crate::stdlib::json_to_vm_value(&policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_contains_the_typed_floor_and_thresholds() {
        let projected = approval_review_default_policy_builtin(&[], &mut String::new())
            .expect("bundled policy projects");
        let policy = projected.as_dict().expect("policy dict");
        assert!(matches!(policy.get("version"), Some(VmValue::Int(1))));

        let floor = policy
            .get("floor")
            .and_then(VmValue::as_dict)
            .and_then(|floor| floor.get("never_grant"))
            .and_then(|value| match value {
                VmValue::List(items) => Some(items),
                _ => None,
            })
            .expect("non-null floor projection");
        assert!(
            !floor.is_empty(),
            "the projected floor must not read as absent"
        );

        let critical = policy
            .get("verdict")
            .and_then(VmValue::as_dict)
            .and_then(|verdict| verdict.get("thresholds"))
            .and_then(VmValue::as_dict)
            .and_then(|thresholds| thresholds.get("critical"))
            .and_then(|value| match value {
                VmValue::String(text) => Some(text.as_ref()),
                _ => None,
            });
        assert_eq!(critical, Some("never"));
    }
}
