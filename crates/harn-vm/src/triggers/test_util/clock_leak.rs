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
//! unchanged when no mock is installed) and, when a mock is active,
//! records the capability id so the operator can see exactly which
//! capability bypassed the mock.
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
//! - Recording is gated on [`super::clock::is_mocked`]: outside a
//!   testbench session the audit is a single boolean check and a
//!   real-clock read. Production pays nothing.
//! - The recorded set is process-global so capabilities running on a
//!   tokio worker thread (which has no thread-local mock state of its
//!   own — the mock lives on the dispatching thread) still surface in
//!   the audit. Tests that run sessions concurrently must therefore be
//!   single-threaded; the testbench session contract already requires
//!   serial setup, and the [`super::clock`] mock stack is thread-local
//!   for the same reason.
//! - [`drain`] empties the registry. [`crate::testbench::TestbenchSession::finalize`]
//!   calls it so each session reports the leaks it observed.

use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use super::clock as clock_inner;

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

struct LeakRegistry {
    /// Insertion-ordered entries keyed by `capability_id`. We use a
    /// `Vec` instead of a `HashMap` so the report is stable across runs
    /// (which matters for tape fidelity diagnostics) and there are
    /// rarely more than a handful of distinct entries.
    entries: Vec<ClockLeak>,
}

impl LeakRegistry {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn record(&mut self, capability_id: &str) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.capability_id == capability_id)
        {
            entry.count = entry.count.saturating_add(1);
            return;
        }
        self.entries.push(ClockLeak {
            capability_id: capability_id.to_string(),
            count: 1,
        });
    }
}

static REGISTRY: Mutex<LeakRegistry> = Mutex::new(LeakRegistry::new());

/// Read the system wall clock, recording a leak entry when a testbench
/// mock is installed. Always returns the real value so production
/// callers (which never run with a mock) are unaffected.
pub fn wall_now(capability_id: &str) -> SystemTime {
    record_if_mocked(capability_id);
    SystemTime::now()
}

/// Read the system monotonic clock, recording a leak entry when a
/// testbench mock is installed. Always returns the real value.
pub fn instant_now(capability_id: &str) -> Instant {
    record_if_mocked(capability_id);
    Instant::now()
}

/// Snapshot the registry without clearing it. Useful for builtins that
/// expose the current leak set to the script (`testbench_clock_leaks()`)
/// so the script can observe and assert without disturbing later
/// `finalize()` consumption.
pub fn snapshot() -> Vec<ClockLeak> {
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .entries
        .clone()
}

/// Drain and return the registry. Called by
/// [`crate::testbench::TestbenchSession::finalize`] so each session
/// reports a self-contained leak set.
pub fn drain() -> Vec<ClockLeak> {
    std::mem::take(
        &mut REGISTRY
            .lock()
            .expect("clock leak registry mutex poisoned")
            .entries,
    )
}

/// Reset the registry. Production code should rely on [`drain`] to
/// consume entries; this is the catch-all called by
/// [`crate::reset_thread_local_state`] and the per-session install path
/// so a fresh session starts with an empty registry.
pub fn reset() {
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .entries
        .clear();
}

fn record_if_mocked(capability_id: &str) {
    if !clock_inner::is_mocked() {
        return;
    }
    REGISTRY
        .lock()
        .expect("clock leak registry mutex poisoned")
        .record(capability_id);
}

/// Test-only serialization mutex. The registry is process-global, so
/// every test that touches it must lock this so they don't race. Shared
/// with the testbench module's tests via [`crate::clock_mock::leak_audit`].
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock_mock::{install_override, MockClock};

    fn isolated_registry<F: FnOnce()>(f: F) {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset();
        f();
        reset();
    }

    #[test]
    fn no_mock_no_leak() {
        isolated_registry(|| {
            let _ = wall_now("test/cap");
            let _ = instant_now("test/cap");
            assert!(snapshot().is_empty());
        });
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
}
