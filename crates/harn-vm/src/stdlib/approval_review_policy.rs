//! Runtime projection of the typed approval-review policy into Harn.

use crate::orchestration::ApprovalReviewPolicy;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&APPROVAL_REVIEW_POLICY_BUILTIN_DEF];

pub(crate) fn register_approval_review_policy_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, MODULE_BUILTINS);
}

/// Normalize an overlay against the one parsed policy owner before projection.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__approval_review_policy(policy?: dict) -> @APPROVAL_REVIEW_POLICY",
    category = "agent.policy"
)]
fn approval_review_policy_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let policy = serde_json::to_value(ApprovalReviewPolicy::bundled()).map_err(|error| {
        VmError::Runtime(format!("serialize bundled approval-review policy: {error}"))
    })?;
    let defaults = crate::stdlib::json_to_vm_value(&policy);
    let merged =
        super::collections::deep_merge_value(&defaults, args.first().unwrap_or(&VmValue::Nil))?;
    let merged = crate::llm::vm_value_to_json_strict(&merged, "approval_review_policy")
        .map_err(VmError::TypeError)?;
    let policy: ApprovalReviewPolicy = serde_json::from_value(merged)
        .map_err(|error| VmError::TypeError(format!("invalid approval-review policy: {error}")))?;
    let policy = serde_json::to_value(policy).map_err(|error| {
        VmError::Runtime(format!(
            "serialize resolved approval-review policy: {error}"
        ))
    })?;
    Ok(crate::stdlib::json_to_vm_value(&policy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_contains_the_typed_floor_and_thresholds() {
        let projected = approval_review_policy_builtin(&[], &mut String::new())
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
