//! Script-side introspection builtins for the testbench.
//!
//! These builtins let a running Harn script query the active testbench
//! axes — whether the clock is mocked, what the overlay filesystem has
//! changed so far — without requiring the script to be wired to the
//! `harn test-bench` CLI. They're no-ops when no testbench session is
//! active, so they're always safe to call.

use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::testbench::overlay_fs::{self, DiffKind};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_testbench_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TESTBENCH_IS_ACTIVE_IMPL_DEF,
    &TESTBENCH_FS_DIFF_IMPL_DEF,
    &TESTBENCH_CLOCK_LEAKS_IMPL_DEF,
];

#[harn_builtin(sig = "testbench_is_active() -> bool", category = "testbench")]
fn testbench_is_active_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(crate::clock_mock::is_mocked()))
}

#[harn_builtin(sig = "testbench_fs_diff() -> list", category = "testbench")]
fn testbench_fs_diff_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(overlay) = overlay_fs::active_overlay() else {
        return Ok(VmValue::List(std::sync::Arc::new(Vec::new())));
    };
    let diff = overlay.diff();
    let entries: Vec<VmValue> = diff
        .into_iter()
        .map(|entry| {
            let mut d = BTreeMap::new();
            d.insert(
                "path".to_string(),
                VmValue::String(std::sync::Arc::from(entry.path.to_string_lossy().as_ref())),
            );
            let (kind_str, content_val) = match entry.kind {
                DiffKind::Added { content } => (
                    "added",
                    VmValue::String(std::sync::Arc::from(
                        String::from_utf8_lossy(&content).as_ref(),
                    )),
                ),
                DiffKind::Modified { content } => (
                    "modified",
                    VmValue::String(std::sync::Arc::from(
                        String::from_utf8_lossy(&content).as_ref(),
                    )),
                ),
                DiffKind::Deleted => ("deleted", VmValue::Nil),
            };
            d.insert(
                "kind".to_string(),
                VmValue::String(std::sync::Arc::from(kind_str)),
            );
            d.insert("content".to_string(), content_val);
            VmValue::Dict(std::sync::Arc::new(d))
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

// Snapshot of the leak audit registry so a script can assert that a
// capability either *did* or *did not* observe real wall-clock time
// during the run. Returns `[{capability, count}]` in the order each
// capability first surfaced. The list survives `finalize()` only
// until the next session installs (which calls `leak_audit::reset`),
// so scripts typically read it just before they end.
#[harn_builtin(sig = "testbench_clock_leaks() -> list", category = "testbench")]
fn testbench_clock_leaks_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let leaks = crate::clock_mock::leak_audit::snapshot();
    let entries: Vec<VmValue> = leaks
        .into_iter()
        .map(|leak| {
            let mut d = BTreeMap::new();
            d.insert(
                "capability".to_string(),
                VmValue::String(std::sync::Arc::from(leak.capability_id)),
            );
            d.insert("count".to_string(), VmValue::Int(leak.count as i64));
            VmValue::Dict(std::sync::Arc::new(d))
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}
