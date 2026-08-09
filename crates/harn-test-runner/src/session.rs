use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;

use crate::reporting::SuiteModulePreparation;

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
    callable_preparations: Mutex<(usize, usize)>,
}

impl Default for TestRunSession {
    fn default() -> Self {
        Self {
            prepared_module_cache: harn_vm::PreparedModuleCache::default(),
            workers: Mutex::new(0),
            clock: harn_vm::clock::RealClock::arc(),
            stdio_available: true,
            callable_preparations: Mutex::new((0, 0)),
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
    pub test_files_compiled: usize,
    pub test_entries_compiled: usize,
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
        let callable = *self.callable_preparations.lock().unwrap();
        TestRunSessionStats {
            workers: *self.workers.lock().unwrap(),
            hits: stats.hits,
            misses: stats.misses,
            insertions: stats.insertions,
            evictions: stats.evictions,
            entries: stats.entries,
            test_files_compiled: callable.0,
            test_entries_compiled: callable.1,
        }
    }

    #[doc(hidden)]
    pub fn prepared_module_cache(&self, worker_index: usize) -> harn_vm::PreparedModuleCache {
        let mut workers = self.workers.lock().unwrap();
        *workers = (*workers).max(worker_index.saturating_add(1));
        self.prepared_module_cache.clone()
    }

    #[doc(hidden)]
    pub fn prepare_import_graphs(
        &self,
        roots: impl IntoIterator<Item = (PathBuf, bool)>,
    ) -> SuiteModulePreparation {
        let mut user_files = BTreeSet::new();
        let mut trusted_files = BTreeSet::new();
        for (path, trusted_host_dispatch) in roots {
            if trusted_host_dispatch {
                trusted_files.insert(path);
            } else {
                user_files.insert(path);
            }
        }
        let user = self.prepare_import_graph(&user_files.into_iter().collect::<Vec<_>>(), false);
        let trusted =
            self.prepare_import_graph(&trusted_files.into_iter().collect::<Vec<_>>(), true);
        SuiteModulePreparation {
            duration_ms: user.duration_ms.saturating_add(trusted.duration_ms),
            modules: user.modules.saturating_add(trusted.modules),
        }
    }

    fn prepare_import_graph(
        &self,
        roots: &[std::path::PathBuf],
        trusted_host_dispatch: bool,
    ) -> SuiteModulePreparation {
        let started_ms = self.clock.monotonic_ms();
        let modules = if trusted_host_dispatch {
            self.prepared_module_cache
                .prepare_trusted_host_dispatch_import_graph(roots)
        } else {
            self.prepared_module_cache.prepare_import_graph(roots)
        };
        let duration_ms = self.clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64;
        SuiteModulePreparation {
            duration_ms,
            modules,
        }
    }

    #[doc(hidden)]
    pub fn record_callable_preparation(&self, files: usize, entries: usize) {
        let mut totals = self.callable_preparations.lock().unwrap();
        totals.0 = totals.0.saturating_add(files);
        totals.1 = totals.1.saturating_add(entries);
    }

    #[doc(hidden)]
    pub fn stdio_available(&self) -> bool {
        self.stdio_available
    }
}
