//! The run tape's per-tool-call file effects, from the same attribution scan
//! the checkpoint store uses. Works in any directory, git or not.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use harn_vm::run_tape::{EffectScan, ToolEffectObserver};

use super::scan::{diff, rescan, scan_tree, ScanChange};
use super::{scan_blind_to, Scan, MAX_STORED_FILE_BYTES};

/// The scanner a run tape uses for per-call file effects; pass it in
/// `harn_vm::run_tape::TapeWorkspace`.
pub fn tape_effect_observer() -> Box<dyn ToolEffectObserver> {
    Box::<TapeEffectObserver>::default()
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
        let after = rescan(&before, root, false);
        let mut scan = EffectScan {
            changes: Vec::new(),
            incomplete: before.unlocated_error || after.unlocated_error,
        };
        for (path, change) in diff(root, &before, &after) {
            if scan_blind_to(&before, &path) || scan_blind_to(&after, &path) {
                scan.incomplete = true;
            }
            match change {
                ScanChange::Deleted => scan.changes.push((path, None)),
                // Replaying an unseen path as a delete would lose it.
                ScanChange::Unseen => scan.incomplete = true,
                ScanChange::Created | ScanChange::Modified => {
                    let full = root.join(&path);
                    if full
                        .metadata()
                        .is_ok_and(|meta| meta.len() > MAX_STORED_FILE_BYTES)
                    {
                        scan.incomplete = true;
                        continue;
                    }
                    match std::fs::read(full) {
                        Ok(bytes) => scan.changes.push((path, Some(bytes))),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            scan.changes.push((path, None));
                        }
                        Err(_) => scan.incomplete = true,
                    }
                }
            }
        }
        scan.changes.sort_by(|a, b| a.0.cmp(&b.0));
        scan
    }
}
