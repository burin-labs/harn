//! Keyspace storage for rate-limit cells.
//!
//! The store sits between the [`super::LimitRegistry`] and the underlying
//! [`super::RateAlgorithm`] cells: it owns the keyed map and clones a
//! fresh cell on demand using the route's algorithm choice. The
//! in-memory implementation is the only one that ships with the
//! primitive today — the trait exists so a Redis-backed counter can
//! land later without a wire-API churn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use harn_clock::Clock;

use super::algorithms::{Algorithm, CellDecision, RateAlgorithm};

/// One bucket dimension on a route (per-tenant, per-scope, per-route).
/// Stored with the rate / capacity so the store can lazily instantiate
/// the right algorithm on first access.
#[derive(Clone, Debug)]
pub struct BucketSpec {
    pub algorithm: Algorithm,
    pub rate_per_sec: f64,
    pub capacity: u32,
}

impl BucketSpec {
    pub fn new(algorithm: Algorithm, rate_per_sec: f64, capacity: u32) -> Self {
        Self {
            algorithm,
            rate_per_sec: rate_per_sec.max(0.0),
            capacity: capacity.max(1),
        }
    }
}

/// Storage trait. Implementations own the keyed-cell map and the
/// algorithm instantiation; callers (the registry) ask "would this key
/// admit one unit of work now?" and surface the answer.
///
/// Implementations must be safe to call concurrently — the in-memory
/// impl uses a mutex per cell-keyspace; a future Redis impl would push
/// the atomicity into the server.
pub trait LimitStore: Send + Sync {
    /// Attempt to admit `cost` units of work against `key`. The cell is
    /// created on first access using `spec`. `cost` is unused today
    /// (callers always pass 1) but reserved so a future variant can
    /// charge an LLM call its token count up front.
    fn try_admit(&self, key: &str, spec: &BucketSpec, cost: u32) -> CellDecision;
}

/// Single-node in-memory store. Each cell is a boxed
/// [`RateAlgorithm`] guarded by a per-cell mutex (so concurrent
/// dispatches against the *same* bucket serialise on the bucket, not
/// the whole map).
/// Shared, mutex-protected handle to one keyspace cell. The double
/// indirection (`Arc<Mutex<…>>`) lets the store hand out a clone of the
/// cell handle without holding the outer map mutex during the per-cell
/// check, so concurrent dispatches against distinct keys never
/// contend on the map.
type SharedCell = Arc<Mutex<Box<dyn RateAlgorithm>>>;

pub struct InMemoryLimitStore {
    clock: Arc<dyn Clock>,
    cells: Mutex<HashMap<String, SharedCell>>,
}

impl std::fmt::Debug for InMemoryLimitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.cells.lock().map(|map| map.len()).unwrap_or(0);
        f.debug_struct("InMemoryLimitStore")
            .field("cells", &len)
            .finish()
    }
}

impl InMemoryLimitStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// Cell count — testing aid for cache pressure regressions.
    pub fn cell_count(&self) -> usize {
        self.cells.lock().expect("limit cells poisoned").len()
    }

    fn cell_handle(&self, key: &str, spec: &BucketSpec, now_ms: i64) -> SharedCell {
        // Fast path: borrowed lookup avoids the `to_string()` alloc on
        // every dispatch against an already-known key. The slow path
        // only fires the first time a route/tenant/scope appears.
        let mut cells = self.cells.lock().expect("limit cells poisoned");
        if let Some(handle) = cells.get(key) {
            return handle.clone();
        }
        let cell = spec
            .algorithm
            .new_cell(spec.rate_per_sec, spec.capacity, now_ms);
        let handle = Arc::new(Mutex::new(cell));
        cells.insert(key.to_string(), handle.clone());
        handle
    }
}

impl LimitStore for InMemoryLimitStore {
    fn try_admit(&self, key: &str, spec: &BucketSpec, _cost: u32) -> CellDecision {
        let now_ms = self.clock.monotonic_ms();
        let handle = self.cell_handle(key, spec, now_ms);
        let mut cell = handle.lock().expect("limit cell poisoned");
        cell.try_admit(now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use harn_clock::PausedClock;
    use time::OffsetDateTime;

    fn clock() -> Arc<PausedClock> {
        PausedClock::new(OffsetDateTime::UNIX_EPOCH)
    }

    #[test]
    fn in_memory_store_admits_burst_then_rejects() {
        let store = InMemoryLimitStore::new(clock());
        let spec = BucketSpec::new(Algorithm::TokenBucket, 1.0, 3);

        for _ in 0..3 {
            assert!(matches!(
                store.try_admit("t:a", &spec, 1),
                CellDecision::Allowed
            ));
        }
        let rejected = store.try_admit("t:a", &spec, 1);
        assert!(matches!(rejected, CellDecision::Rejected { .. }));

        // Distinct key has its own cell.
        assert!(matches!(
            store.try_admit("t:b", &spec, 1),
            CellDecision::Allowed
        ));
    }

    #[test]
    fn in_memory_store_recovers_after_clock_advance() {
        let clock = clock();
        let store = InMemoryLimitStore::new(clock.clone());
        let spec = BucketSpec::new(Algorithm::TokenBucket, 1.0, 1);

        assert!(matches!(
            store.try_admit("t:a", &spec, 1),
            CellDecision::Allowed
        ));
        assert!(matches!(
            store.try_admit("t:a", &spec, 1),
            CellDecision::Rejected { .. }
        ));
        clock.advance(Duration::from_secs(1));
        assert!(matches!(
            store.try_admit("t:a", &spec, 1),
            CellDecision::Allowed
        ));
    }
}
