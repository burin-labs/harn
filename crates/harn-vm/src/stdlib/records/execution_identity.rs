//! Execution identity attachment for newly constructed run records.

use crate::orchestration::{normalize_run_record, RunRecord, EXECUTION_EVIDENCE_SCHEMA_VERSION};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::to_vm;

#[harn_builtin(
    exposure = "harness.obs.run_record",
    effects = ["state.read@const=execution-scope"],
    sig = "run_record(payload: dict) -> dict", category = "records"
)]
pub(super) fn run_record_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut run = normalize_run_record(
        args.first()
            .ok_or_else(|| VmError::Runtime("run_record: missing payload".to_string()))?,
    )?;
    attach_active_execution_identity(&mut run);
    to_vm(&run)
}

fn attach_active_execution_identity(run: &mut RunRecord) {
    if run.evidence.execution_id.is_some() {
        return;
    }
    let Some(execution_id) = crate::current_execution_scope() else {
        return;
    };
    run.evidence.schema_version = EXECUTION_EVIDENCE_SCHEMA_VERSION;
    run.evidence.execution_id = Some(execution_id.to_string());
    run.evidence
        .gaps
        .retain(|gap| gap.component != "execution_identity" || gap.code != "legacy_missing");
}
