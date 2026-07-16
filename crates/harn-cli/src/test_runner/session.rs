use std::sync::Mutex;

/// Reusable runtime state for repeated user-test runs.
///
/// Each logical worker retains its own bounded prepared-module cache while
/// every test case still receives a fresh VM and fresh runtime state. Keeping
/// the cache per worker avoids introducing cross-worker contention while
/// allowing watch mode and long-lived embedders to amortize module hydration.
#[derive(Default)]
pub struct TestRunSession {
    prepared_module_caches: Mutex<Vec<harn_vm::PreparedModuleCache>>,
}

/// Aggregate prepared-module cache counters for a [`TestRunSession`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    pub fn stats(&self) -> TestRunSessionStats {
        self.prepared_module_caches
            .lock()
            .unwrap()
            .iter()
            .map(harn_vm::PreparedModuleCache::stats)
            .fold(TestRunSessionStats::default(), |mut total, stats| {
                total.workers += 1;
                total.hits = total.hits.saturating_add(stats.hits);
                total.misses = total.misses.saturating_add(stats.misses);
                total.insertions = total.insertions.saturating_add(stats.insertions);
                total.evictions = total.evictions.saturating_add(stats.evictions);
                total.entries = total.entries.saturating_add(stats.entries);
                total
            })
    }

    pub(super) fn prepared_module_cache(
        &self,
        worker_index: usize,
    ) -> harn_vm::PreparedModuleCache {
        let mut caches = self.prepared_module_caches.lock().unwrap();
        caches.resize_with(worker_index.saturating_add(1), Default::default);
        caches[worker_index].clone()
    }
}
