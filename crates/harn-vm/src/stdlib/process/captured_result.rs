use std::collections::BTreeMap;

use super::CapturedRun;
use crate::value::{VmDictExt, VmValue};

/// Build the captured-command result, including interruption and verifier evidence.
pub(super) fn captured_run_to_value(run: &CapturedRun) -> VmValue {
    let status = if run.timed_out || run.interrupted {
        -1
    } else {
        run.output.status.code().unwrap_or(-1) as i64
    };
    let success = !run.timed_out && !run.interrupted && run.output.status.success();
    let mut result = BTreeMap::new();
    result.put_str(
        "stdout",
        String::from_utf8_lossy(&run.output.stdout).as_ref(),
    );
    result.put_str(
        "stderr",
        String::from_utf8_lossy(&run.output.stderr).as_ref(),
    );
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("success".to_string(), VmValue::Bool(success));
    result.insert("timed_out".to_string(), VmValue::Bool(run.timed_out));
    result.insert("duration_ms".to_string(), VmValue::Int(run.duration_ms));
    if let Some(id) = &run.source_verifier_id {
        result.put_str("source_verifier_id", id);
    }
    VmValue::dict(result)
}
