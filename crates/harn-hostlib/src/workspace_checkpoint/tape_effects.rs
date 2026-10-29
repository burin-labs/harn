//! The run tape's per-tool-call file effects, from the same attribution scan
//! the checkpoint store uses. Works in any directory, git or not.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use harn_vm::run_tape::{EffectScan, ToolEffectObserver};

use super::{scan::scan_tree, scan_blind_to, Scan, MAX_STORED_FILE_BYTES};

/// Give the run tape's per-call file effects this module's scanner.
pub(super) fn register() {
    harn_vm::run_tape::register_tool_effect_observer(Box::<TapeEffectObserver>::default());
}

#[derive(Default)]
struct TapeEffectObserver {
    pending: Mutex<HashMap<String, Arc<Scan>>>,
}

impl ToolEffectObserver for TapeEffectObserver {
    fn begin(&self, root: &Path, call_id: &str) {
        let scan = Arc::new(scan_tree(root, false));
        self.pending
            .lock()
            .expect("tape effect observer poisoned")
            .insert(call_id.to_string(), scan);
    }

    fn end(&self, root: &Path, call_id: &str) -> EffectScan {
        let Some(before) = self
            .pending
            .lock()
            .expect("tape effect observer poisoned")
            .remove(call_id)
        else {
            return EffectScan {
                changes: Vec::new(),
                incomplete: true,
            };
        };
        let after = scan_tree(root, false);
        let mut scan = EffectScan {
            changes: Vec::new(),
            incomplete: before.unlocated_error || after.unlocated_error,
        };
        for (path, entry) in &after.index {
            if before.index.get(path) == Some(entry) {
                continue;
            }
            if scan_blind_to(&before, path) || scan_blind_to(&after, path) {
                scan.incomplete = true;
            }
            if entry.size > MAX_STORED_FILE_BYTES {
                scan.incomplete = true;
                continue;
            }
            match std::fs::read(root.join(path)) {
                Ok(bytes) => scan.changes.push((path.clone(), Some(bytes))),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    scan.changes.push((path.clone(), None));
                }
                Err(_) => scan.incomplete = true,
            }
        }
        for path in before.index.keys() {
            if !after.index.contains_key(path) {
                scan.changes.push((path.clone(), None));
            }
        }
        scan.changes.sort_by(|a, b| a.0.cmp(&b.0));
        scan
    }
}
