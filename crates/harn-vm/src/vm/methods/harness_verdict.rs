//! `harness.verdict.*` VM-side methods — the issuance authority for positive
//! verdicts (PR2 verdict-integrity class-kill). Split out of the (grandfathered
//! oversized) `harness.rs` per the source-file-length ratchet; the verdict
//! capability is a cohesive unit whose opaque-receipt handling belongs together.

use crate::harness::VmHarness;
use crate::value::{VmDictExt, VmError, VmValue, VmVerdictReceipt};
use crate::vm::methods::harness::{method_unsupported, tag_sandbox_denied};

impl crate::vm::Vm {
    /// Dispatch `harness.verdict.*` in real mode — the issuance authority for
    /// positive verdicts. `issue(result_handle)` resolves the host-owned record
    /// of a real `run_test` execution via the hostlib builtin, then — ONLY when
    /// the host's frozen disposition for that execution is green — mints the
    /// opaque `VerdictReceipt` VM-side. The receipt is non-serializable, so it
    /// must be constructed here, after the hostlib boundary, never returned
    /// across the hostlib response-schema seam. A caller cannot assert a pass nor
    /// fabricate the evidence: the disposition is the host's, computed from an
    /// execution it ran, not a caller scalar or a caller-authored file.
    pub(in crate::vm) async fn call_harness_verdict_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "issue" => {
                let raw = self
                    .call_named_builtin("__harness_verdict_issue", args.to_vec())
                    .await
                    .map_err(tag_sandbox_denied)?;
                Ok(Self::mint_verdict_receipt(&raw))
            }
            // `same_run` operates on the OPAQUE receipts directly — they cannot
            // cross the hostlib boundary (non-serializable), so it is handled
            // VM-side, never via a hostlib builtin.
            "same_run" => Ok(Self::verdict_same_run(args)),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    /// True iff every argument (or every element of a single list argument) is a
    /// `VerdictReceipt` sharing one execution scope AND that scope is the
    /// currently-active owner. The stdlib `verdict_all` cross-run guard rides
    /// this: receipts produced by different runs cannot be combined (the temporal
    /// cousin of the fabricated-proof class), and — fail-closed — aggregation
    /// must happen INSIDE the run that produced them, so a receipt cannot be
    /// replayed into a later execution. A non-receipt element, a scope mismatch,
    /// an empty set, or no/other active scope yields `false`.
    fn verdict_same_run(args: &[VmValue]) -> VmValue {
        let items: Vec<&VmValue> = match args.first() {
            Some(VmValue::List(list)) if args.len() == 1 => list.iter().collect(),
            _ => args.iter().collect(),
        };
        if items.is_empty() {
            return VmValue::Bool(false);
        }
        let mut scope: Option<crate::ExecutionId> = None;
        for item in items {
            let VmValue::VerdictReceipt(receipt) = item else {
                return VmValue::Bool(false);
            };
            match &scope {
                None => scope = Some(receipt.execution_scope.clone()),
                Some(first) => {
                    if *first != receipt.execution_scope {
                        return VmValue::Bool(false);
                    }
                }
            }
        }
        // Fail-closed: the receipts' shared scope must be the ACTIVE owner.
        let active = crate::observability::execution_scope::current_execution_scope();
        match (active, scope) {
            (Some(a), Some(s)) if a == s => VmValue::Bool(true),
            _ => VmValue::Bool(false),
        }
    }

    /// Mint the opaque `VerdictReceipt` VM-side from the host validator's plain
    /// dict. On any non-`"pass"` outcome the dict passes through unchanged (no
    /// receipt), so the `.harn` side maps outcome -> fail / indeterminate. The
    /// run identity is the host-controlled ambient request id — a caller cannot
    /// forge it — which, with the host-captured content hash, closes the cross-run
    /// replay class on top of the execution-provenance floor the hostlib enforces.
    fn mint_verdict_receipt(raw: &VmValue) -> VmValue {
        let Some(dict) = raw.as_dict() else {
            return raw.clone();
        };
        let outcome = dict
            .get("outcome")
            .map(|v| v.as_str_cow())
            .unwrap_or_default();
        if outcome.as_ref() != "pass" {
            return raw.clone();
        }
        let field_str = |k: &str| -> String {
            dict.get(k)
                .map(|v| v.as_str_cow().into_owned())
                .unwrap_or_default()
        };
        let field_int = |k: &str| -> i64 { dict.get(k).and_then(|v| v.as_int()).unwrap_or(0) };
        let artifact_id = field_str("artifact_id");
        let artifact_hash = field_str("artifact_hash");
        let plan_id = field_str("plan_id");
        let workspace_hash = field_str("workspace_hash");
        let command_hash = field_str("command_hash");
        let execution_scope = field_str("execution_scope");
        let passed = field_int("passed").max(0) as u32;
        let total = field_int("total").max(0) as u32;
        if artifact_id.is_empty()
            || artifact_hash.is_empty()
            || plan_id.is_empty()
            || workspace_hash.is_empty()
            || command_hash.is_empty()
            || execution_scope.is_empty()
            || passed == 0
            || total < passed
        {
            let mut unavailable = dict.clone();
            unavailable.put_str("outcome", "unavailable");
            unavailable.put_str(
                "detail",
                "host verdict response lacked complete authorized execution provenance",
            );
            return VmValue::dict_map(unavailable);
        }
        let subject_raw = field_str("subject");
        let subject = if subject_raw.is_empty() {
            None
        } else {
            Some(std::sync::Arc::from(subject_raw.as_str()))
        };
        // The receipt is bound to the execution that PRODUCED the evidence,
        // captured by the hostlib validator at `run_test` record time and passed
        // through on the `"pass"` dict — NOT to a consumption-time request id. A
        // `"pass"` is only reached after the hostlib scope gate confirmed the
        // active scope equals this owner, so it is always present here.
        let Ok(execution_scope) = crate::ExecutionId::parse(&execution_scope) else {
            let mut unavailable = dict.clone();
            unavailable.put_str("outcome", "unavailable");
            unavailable.put_str(
                "detail",
                "host verdict response carried an invalid execution identity",
            );
            return VmValue::dict_map(unavailable);
        };
        let receipt = VmVerdictReceipt {
            artifact_id: std::sync::Arc::from(artifact_id.as_str()),
            content_hash: std::sync::Arc::from(artifact_hash.as_str()),
            plan_id: std::sync::Arc::from(plan_id.as_str()),
            workspace_hash: std::sync::Arc::from(workspace_hash.as_str()),
            command_hash: std::sync::Arc::from(command_hash.as_str()),
            passed,
            total,
            execution_scope,
            subject,
        };
        let mut out = dict.clone();
        out.put("receipt", VmValue::verdict_receipt(receipt));
        VmValue::dict_map(out)
    }
}

#[cfg(test)]
mod verdict_same_run_tests {
    use crate::observability::execution_scope::{enter_execution_scope, ExecutionId};
    use crate::value::{DictMap, VmDictExt, VmValue, VmVerdictReceipt};
    use std::sync::Arc;

    fn receipt(scope: &ExecutionId) -> VmValue {
        VmValue::verdict_receipt(VmVerdictReceipt {
            artifact_id: Arc::from("art"),
            content_hash: Arc::from("sha256:abc"),
            plan_id: Arc::from("sha256:plan"),
            workspace_hash: Arc::from("sha256:workspace"),
            command_hash: Arc::from("sha256:command"),
            passed: 1,
            total: 1,
            execution_scope: scope.clone(),
            subject: None,
        })
    }

    #[test]
    fn same_scope_receipts_combine_inside_that_scope() {
        let owner = ExecutionId::mint();
        let _scope = enter_execution_scope(owner.clone());
        let out = crate::vm::Vm::verdict_same_run(&[receipt(&owner), receipt(&owner)]);
        assert!(matches!(out, VmValue::Bool(true)));
    }

    #[test]
    fn cross_scope_receipts_are_rejected() {
        // The cross-run replay negative: a pass produced by run A cannot combine
        // with one from run B — verdict_all degrades such an aggregate to
        // indeterminate off this `false`.
        let owner = ExecutionId::mint();
        let other = ExecutionId::mint();
        let _scope = enter_execution_scope(owner.clone());
        let out = crate::vm::Vm::verdict_same_run(&[receipt(&owner), receipt(&other)]);
        assert!(matches!(out, VmValue::Bool(false)));
    }

    #[test]
    fn same_scope_but_no_active_scope_is_rejected() {
        // Fail-closed: aggregation outside any owning run cannot combine, even
        // when the receipts agree — a replay into a later, scope-less context.
        let owner = ExecutionId::mint();
        let out = crate::vm::Vm::verdict_same_run(&[receipt(&owner), receipt(&owner)]);
        assert!(matches!(out, VmValue::Bool(false)));
    }

    #[test]
    fn same_scope_but_different_active_scope_is_rejected() {
        // Replay into a DIFFERENT active run is rejected: the receipts' owner is
        // not the active owner.
        let owner = ExecutionId::mint();
        let other = ExecutionId::mint();
        let _scope = enter_execution_scope(other);
        let out = crate::vm::Vm::verdict_same_run(&[receipt(&owner), receipt(&owner)]);
        assert!(matches!(out, VmValue::Bool(false)));
    }

    #[test]
    fn a_non_receipt_element_is_rejected() {
        let owner = ExecutionId::mint();
        let _scope = enter_execution_scope(owner.clone());
        let out = crate::vm::Vm::verdict_same_run(&[receipt(&owner), VmValue::Int(42)]);
        assert!(matches!(out, VmValue::Bool(false)));
    }

    #[test]
    fn empty_is_rejected() {
        assert!(matches!(
            crate::vm::Vm::verdict_same_run(&[]),
            VmValue::Bool(false)
        ));
    }

    #[test]
    fn malformed_host_identity_cannot_become_an_opaque_receipt() {
        let mut raw = DictMap::new();
        for (field, value) in [
            ("outcome", "pass"),
            ("artifact_id", "artifact"),
            ("artifact_hash", "sha256:artifact"),
            ("plan_id", "sha256:plan"),
            ("workspace_hash", "sha256:workspace"),
            ("command_hash", "sha256:command"),
            ("execution_scope", "external-run"),
        ] {
            raw.put_str(field, value);
        }
        raw.put("passed", VmValue::Int(1));
        raw.put("total", VmValue::Int(1));

        let result = crate::vm::Vm::mint_verdict_receipt(&VmValue::dict_map(raw));
        let result = result.as_dict().expect("unavailable response dict");
        assert_eq!(
            result.get("outcome").map(VmValue::as_str_cow).as_deref(),
            Some("unavailable")
        );
        assert!(!result.contains_key("receipt"));
    }
}
