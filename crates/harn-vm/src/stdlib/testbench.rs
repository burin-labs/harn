//! Script-side introspection builtins for the testbench.
//!
//! These builtins let a running Harn script query the active testbench
//! axes — whether the clock is mocked, what the overlay filesystem has
//! changed so far — without requiring the script to be wired to the
//! `harn test-bench` CLI. They're no-ops when no testbench session is
//! active, so they're always safe to call.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::testbench::overlay_fs::{self, DiffKind};
use crate::value::VmValue;
use crate::vm::Vm;

pub(crate) fn register_testbench_builtins(vm: &mut Vm) {
    vm.register_builtin("testbench_is_active", |_args, _out| {
        Ok(VmValue::Bool(crate::clock_mock::is_mocked()))
    });

    vm.register_builtin("testbench_fs_diff", |_args, _out| {
        let Some(overlay) = overlay_fs::active_overlay() else {
            return Ok(VmValue::List(Rc::new(Vec::new())));
        };
        let diff = overlay.diff();
        let entries: Vec<VmValue> = diff
            .into_iter()
            .map(|entry| {
                let mut d = BTreeMap::new();
                d.insert(
                    "path".to_string(),
                    VmValue::String(Rc::from(entry.path.to_string_lossy().as_ref())),
                );
                let (kind_str, content_val) = match entry.kind {
                    DiffKind::Added { content } => (
                        "added",
                        VmValue::String(Rc::from(String::from_utf8_lossy(&content).as_ref())),
                    ),
                    DiffKind::Modified { content } => (
                        "modified",
                        VmValue::String(Rc::from(String::from_utf8_lossy(&content).as_ref())),
                    ),
                    DiffKind::Deleted => ("deleted", VmValue::Nil),
                };
                d.insert("kind".to_string(), VmValue::String(Rc::from(kind_str)));
                d.insert("content".to_string(), content_val);
                VmValue::Dict(Rc::new(d))
            })
            .collect();
        Ok(VmValue::List(Rc::new(entries)))
    });
}
