//! Runtime audit for host capabilities that observe real wall-clock or
//! monotonic time while a testbench mock clock is active.
//!
//! Re-exported as [`crate::clock_mock::leak_audit`].
//!
//! When a testbench session installs a mock via [`super::clock`], every
//! host capability that consults the OS wall or monotonic clock
//! directly is a fidelity hazard: the value it observes has no
//! relationship to the script's virtual time, so a recorded tape
//! diverges between record and replay even when the rest of the run is
//! deterministic.
//!
//! This module gives capabilities a single seam to route those reads
//! through. The seam returns the real value (so production behavior is
//! unchanged when no audit scope is installed) and, when a testbench
//! session opts into auditing, records the capability id so the operator
//! can see exactly which capability bypassed the mock.
//!
//! ## Usage
//!
//! ```ignore
//! use crate::clock_mock::leak_audit;
//!
//! let started_at = leak_audit::wall_now("host_call/process.exec.started_at");
//! ```
//!
//! ## Lifecycle
//!
//! - Recording is gated on an active [`ClockLeakScope`]: outside a
//!   testbench session the audit is a thread-local scope check and a
//!   real-clock read. Production pays nothing beyond that check.
//! - The registry is process-global so capabilities running on worker
//!   threads can surface in the audit, but entries are keyed by scope.
//!   A reset in one test cannot erase a sibling session's observations.
//! - [`drain`] empties only the current scope.
//!   [`crate::testbench::TestbenchSession::finalize`] calls it so each
//!   session reports the leaks it observed.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// One observed leak: a capability read real time while a testbench
/// mock was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockLeak {
    /// Stable identifier for the capability, e.g.
    /// `"host_call/process.exec.started_at"` or `"stdlib/date_iso"`.
    /// Capability ids are namespaced with `<subsystem>/<surface>` so a
    /// report names the bypassing site unambiguously.
    pub capability_id: String,
    /// Number of times the capability was observed during the session.
    pub count: u64,
}

/// Stable audit scope for one mock-clock testbench session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockLeakScope(u64);

impl ClockLeakScope {
    fn next() -> Self {
        static NEXT_SCOPE_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_SCOPE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

thread_local! {
    static ACTIVE_SCOPE_STACK: RefCell<Vec<ClockLeakScope>> = const { RefCell::new(Vec::new()) };
}

/// Guard that makes a [`ClockLeakScope`] current on this thread.
pub struct ClockLeakScopeGuard {
    scope: ClockLeakScope,
    owner: bool,
}

impl ClockLeakScopeGuard {
    /// The scope this guard installed. Pass it to [`enter_scope`] when a
    /// worker thread must report leaks to the same session.
    pub fn scope(&self) -> ClockLeakScope {
        self.scope
    }
}

impl Drop for ClockLeakScopeGuard {
    fn drop(&mut self) {
        ACTIVE_SCOPE_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.last().copied() == Some(self.scope) {
                stack.pop();
            } else if let Some(index) = stack.iter().rposition(|scope| *scope == self.scope) {
                stack.remove(index);
            }
        });
        if self.owner {
            REGISTRY
                .lock()
                .expect("clock leak registry mutex poisoned")
                .remove_scope(self.scope);
        }
    }
}

/// Start a fresh audit scope on this thread. Owning guards remove any
/// leftover entries on drop, so a panicking test cannot leak observations
/// into a later session.
pub fn install_scope() -> ClockLeakScopeGuard {
    let scope = ClockLeakScope::next();
    ACTIVE_SCOPE_STACK.with(|stack| stack.borrow_mut().push(scope));
    ClockLeakScopeGuard { scope, owner: true }
}

/// Enter an existing audit scope on this thread. This is the explicit
/// propagation hook for worker threads that need to attribute real-clock
/// reads to their parent testbench session.
pub fn enter_scope(scope: ClockLeakScope) -> ClockLeakScopeGuard {
    ACTIVE_SCOPE_STACK.with(|stack| stack.borrow_mut().push(scope));
    ClockLeakScopeGuard {
        scope,
        owner: false,
    }
}

fn active_scope() -> Option<ClockLeakScope> {
    ACTIVE_SCOPE_STACK.with(|stack| stack.borrow().last().copied())
}

struct ScopedLeaks {
    scope: ClockLeakScope,
    entries: Vec<ClockLeak>,
}

struct LeakRegistry {
    scopes: Vec<ScopedLeaks>,
}

impl LeakRegistry {
    const fn new() -> Self {
        Self { scopes: Vec::new() }
    }

    fn entries_mut(&mut self, scope: ClockLeakScope) -> &mut Vec<ClockLeak> {
        if let Some(index) = self.scopes.iter().position(|entry| entry.scope == scope) {
            return &mut self.scopes[index].entries;
        }
        self.scopes.push(ScopedLeaks {
            scope,
            entries: Vec::new(),
        });
        &mut self.scopes.last_mut().expect("scope just pushed").entries
    }

    fn record(&mut self, scope: ClockLeakScope, capability_id: &str) {
        let entries = self.entries_mut(scope);
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.capability_id == capability_id)
        {
            entry.count = entry.count.saturating_add(1);
            return;
        }
        entries.push(ClockLeak {
            capability_id: capability_id.to_string(),
            count: 1,
        });
    }

    fn snapshot(&self, scope: ClockLeakScope) -> Vec<ClockLeak> {
        self.scopes
            .iter()
            .find(|entry| entry.scope == scope)
            .map(|entry| entry.entries.clone())
            .unwrap_or_default()
    }

    fn drain(&mut self, scope: ClockLeakScope) -> Vec<ClockLeak> {
        let Some(index) = self.scopes.iter().position(|entry| entry.scope == scope) else {
            return Vec::new();
        };
        std::mem::take(&mut self.scopes[index].entries)
    }

    fn reset(&mut self, scope: ClockLeakScope) {
        if let Some(entry) = self.scopes.iter_mut().find(|entry| entry.scope == scope) {
            entry.entries.clear();
        }
    }

    fn remove_scope(&mut self, scope: ClockLeakScope) {
        self.scopes.retain(|entry| entry.scope != scope);
    }
}

static REGISTRY: Mutex<LeakRegistry> = Mutex::new(LeakRegistry::new());

/// Read the system wall clock, recording a leak entry when a testbench
/// audit scope is active. Always returns the real value so production
/// callers (which never install a scope) are unaffected.
pub fn wall_now(capability_id: &str) -> SystemTime {
    record_in_active_scope(capability_id);
    SystemTime::now()
}

/// Read the system monotonic clock, recording a leak entry when a
/// testbench audit scope is active. Always returns the real value.
pub fn instant_now(capability_id: &str) -> Instant {
    record_in_active_scope(capability_id);
    Instant::now()
}

/// Snapshot the registry without clearing it. Useful for builtins that
/// expose the current leak set to the script (`testbench_clock_leaks()`)
/// so the script can observe and assert without disturbing later
/// `finalize()` consumption.
pub fn snapshot() -> Vec<ClockLeak> {
    let Some(scope) = active_scope() else {
        return Vec::new();
    };
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .snapshot(scope)
}

/// Drain and return the registry. Called by
/// [`crate::testbench::TestbenchSession::finalize`] so each session
/// reports a self-contained leak set.
pub fn drain() -> Vec<ClockLeak> {
    let Some(scope) = active_scope() else {
        return Vec::new();
    };
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .drain(scope)
}

/// Reset the current scope's registry entries. Production code should
/// rely on [`drain`] to consume entries; this is the catch-all called by
/// [`crate::reset_thread_local_state`] so a reset in one in-process
/// test cannot erase another active session's observations.
pub fn reset() {
    let Some(scope) = active_scope() else {
        return;
    };
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .reset(scope);
}

fn record_in_active_scope(capability_id: &str) {
    let Some(scope) = active_scope() else {
        return;
    };
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .record(scope, capability_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock_mock::{install_override, MockClock};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn isolated_registry<F: FnOnce()>(f: F) {
        let _scope = install_scope();
        reset();
        f();
        reset();
    }

    #[test]
    fn no_mock_no_leak() {
        let _ = wall_now("test/cap");
        let _ = instant_now("test/cap");
        assert!(snapshot().is_empty());
    }

    #[test]
    fn mock_present_records_leak() {
        isolated_registry(|| {
            let _guard = install_override(MockClock::at_wall_ms(1_000_000));
            let _ = wall_now("test/cap");
            let leaks = snapshot();
            assert_eq!(leaks.len(), 1);
            assert_eq!(leaks[0].capability_id, "test/cap");
            assert_eq!(leaks[0].count, 1);
        });
    }

    #[test]
    fn duplicate_capability_increments_count() {
        isolated_registry(|| {
            let _guard = install_override(MockClock::at_wall_ms(1_000_000));
            wall_now("test/cap");
            wall_now("test/cap");
            wall_now("test/cap");
            let leaks = snapshot();
            assert_eq!(leaks.len(), 1);
            assert_eq!(leaks[0].count, 3);
        });
    }

    #[test]
    fn distinct_capabilities_kept_separate_in_insertion_order() {
        isolated_registry(|| {
            let _guard = install_override(MockClock::at_wall_ms(1_000_000));
            wall_now("test/a");
            wall_now("test/b");
            wall_now("test/a");
            let leaks = snapshot();
            assert_eq!(leaks.len(), 2);
            assert_eq!(leaks[0].capability_id, "test/a");
            assert_eq!(leaks[0].count, 2);
            assert_eq!(leaks[1].capability_id, "test/b");
            assert_eq!(leaks[1].count, 1);
        });
    }

    #[test]
    fn drain_empties_registry() {
        isolated_registry(|| {
            let _guard = install_override(MockClock::at_wall_ms(1_000_000));
            wall_now("test/cap");
            let drained = drain();
            assert_eq!(drained.len(), 1);
            assert!(snapshot().is_empty());
        });
    }

    #[test]
    fn sibling_scope_reset_cannot_erase_active_scope() {
        let alpha = install_scope();
        let alpha_scope = alpha.scope();
        let beta = install_scope();
        let beta_scope = beta.scope();

        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            let _beta = enter_scope(beta_scope);
            worker_barrier.wait();
            crate::reset_thread_local_state();
            wall_now("test/beta");
            assert_eq!(snapshot().len(), 1);
        });

        let _alpha_thread = enter_scope(alpha_scope);
        wall_now("test/alpha");
        barrier.wait();
        worker.join().expect("worker");
        let leaks = snapshot();
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].capability_id, "test/alpha");
        assert_eq!(leaks[0].count, 1);
        drop((alpha, beta));
    }
}
