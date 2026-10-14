//! Per-run memo of the import-closure scans `harn check` repeats per file.
//!
//! Preflight and mock-host collection walk every checked file's whole import
//! closure, so a module shared by N checked files used to be walked N times.
//! [`ScanMemo`] stores each module's own scan result (its events, with
//! imports left as references) once per check run, and every dependent file
//! replays it in walk order. Replay, not a merged closure result, keeps each
//! file's diagnostics and recorded filesystem probes identical to an
//! unmemoized walk, which the persistent result cache depends on.
//!
//! The memo is scoped to one check run on purpose. Entries capture filesystem
//! observations (template existence, asset reads), so a process that checks,
//! edits a template, and checks again must start a new memo.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use super::mock_host::MockHostScan;
use super::preflight::ModuleScan;

/// One check run's shared module-scan results, safe to share across driver
/// workers.
#[derive(Default)]
pub(crate) struct ScanMemo {
    /// Preflight events per `(canonical module path, scan context)`. The
    /// context digests the check config and the effective host capability
    /// surface, the only inputs besides the module that a module's preflight
    /// events read. `None` records an unparseable module.
    preflight: MemoMap<(PathBuf, [u8; 32]), Option<Arc<ModuleScan>>>,
    /// Mock-host declarations per canonical module path; context-free.
    mock_host: MemoMap<PathBuf, Option<Arc<MockHostScan>>>,
}

impl ScanMemo {
    pub(super) fn preflight(
        &self,
        canonical: &std::path::Path,
        context: [u8; 32],
        build: impl FnOnce() -> Option<ModuleScan>,
    ) -> Option<Arc<ModuleScan>> {
        self.preflight
            .get_or_init((canonical.to_path_buf(), context), || build().map(Arc::new))
    }

    pub(super) fn mock_host(
        &self,
        canonical: &std::path::Path,
        build: impl FnOnce() -> Option<MockHostScan>,
    ) -> Option<Arc<MockHostScan>> {
        self.mock_host
            .get_or_init(canonical.to_path_buf(), || build().map(Arc::new))
    }
}

/// A concurrent compute-once map: hits take the shared lock, a first sighting
/// takes the exclusive one only to insert an empty slot, and the value is
/// computed outside the map lock so workers building different modules never
/// serialize.
struct MemoMap<K, V> {
    slots: RwLock<HashMap<K, Arc<OnceLock<V>>>>,
}

impl<K, V> Default for MemoMap<K, V> {
    fn default() -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V: Clone> MemoMap<K, V> {
    fn get_or_init(&self, key: K, init: impl FnOnce() -> V) -> V {
        let existing = self
            .slots
            .read()
            .expect("check scan memo lock poisoned")
            .get(&key)
            .cloned();
        let slot = existing.unwrap_or_else(|| {
            self.slots
                .write()
                .expect("check scan memo lock poisoned")
                .entry(key)
                .or_default()
                .clone()
        });
        slot.get_or_init(init).clone()
    }
}
