use std::sync::Mutex;

use serde::Serialize;

use super::reporting::SuiteModulePreparation;

/// Reusable runtime state for repeated user-test runs.
///
/// Logical workers share one bounded cache of immutable prepared bytecode while
/// every test case still receives a fresh VM and fresh runtime state. Sharing
/// lets the suite prepare its import graph once before per-test clocks start,
/// instead of making one arbitrary test on every worker pay the cold compile.
pub struct TestRunSession {
    prepared_module_cache: harn_vm::PreparedModuleCache,
    workers: Mutex<usize>,
    clock: std::sync::Arc<dyn harn_vm::clock::Clock>,
    stdio_available: bool,
}

impl Default for TestRunSession {
    fn default() -> Self {
        Self {
            prepared_module_cache: harn_vm::PreparedModuleCache::default(),
            workers: Mutex::new(0),
            clock: harn_vm::clock::RealClock::arc(),
            stdio_available: true,
        }
    }
}

/// Aggregate prepared-module cache counters for a [`TestRunSession`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TestRunSessionStats {
    pub workers: usize,
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
    pub entries: usize,
}

impl TestRunSession {
    /// Build a session for an embedder whose control protocol owns stdio.
    pub fn without_stdio() -> Self {
        Self {
            stdio_available: false,
            ..Self::default()
        }
    }

    pub fn stats(&self) -> TestRunSessionStats {
        let stats = self.prepared_module_cache.stats();
        TestRunSessionStats {
            workers: *self.workers.lock().unwrap(),
            hits: stats.hits,
            misses: stats.misses,
            insertions: stats.insertions,
            evictions: stats.evictions,
            entries: stats.entries,
        }
    }

    pub(super) fn prepared_module_cache(
        &self,
        worker_index: usize,
    ) -> harn_vm::PreparedModuleCache {
        let mut workers = self.workers.lock().unwrap();
        *workers = (*workers).max(worker_index.saturating_add(1));
        self.prepared_module_cache.clone()
    }

    pub(super) fn prepare_import_graph(
        &self,
        roots: &[std::path::PathBuf],
    ) -> SuiteModulePreparation {
        let started_ms = self.clock.monotonic_ms();
        let modules = self.prepared_module_cache.prepare_import_graph(roots);
        let duration_ms = self.clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64;
        SuiteModulePreparation {
            duration_ms,
            modules,
        }
    }

    pub(super) fn stdio_available(&self) -> bool {
        self.stdio_available
    }
}
