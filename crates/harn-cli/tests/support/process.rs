//! Test-process synchronization sentinels.
//!
//! Earlier revisions of the harn-cli test suite acquired a process-wide
//! `Mutex` and a `flock(2)`-backed file lock at
//! `$TMPDIR/harn-process-tests.lock` before spawning the `harn` binary, in
//! order to "make subprocess tests deterministic." Investigation showed
//! there was no shared state for the lock to guard:
//!
//! - Every test used its own [`tempfile::TempDir`] for the manifest, state
//!   directory, and event-log SQLite path.
//! - Every test bound the orchestrator listener to `127.0.0.1:0`, so the
//!   kernel handed out fresh ephemeral ports for each parallel run.
//! - Secrets and other configuration were passed through the subprocess
//!   environment directly via [`std::process::Command::env`], not via
//!   the parent test process.
//!
//! Meanwhile the cost was substantial: 21 orchestrator-http tests each held
//! the cross-process lock through a full `spawn -> bind -> HTTP round trip
//! -> SIGTERM -> drain` cycle, so the last test in the queue routinely
//! waited 30–40 seconds for its turn before any of its own work began.
//! Under nextest's 60s slow-test ceiling, the tail of the queue tripped
//! `terminate-after` at ~60s wall time even though each test individually
//! ran in ~3s.
//!
//! The fix is the architectural one: stop serializing. Tests are already
//! isolated; let nextest run them in parallel up to the worker thread
//! count. Empirically, the orchestrator-http suite now completes in a
//! fraction of its previous wall-clock time and no individual test
//! starves on lock acquisition.
//!
//! [`HarnProcessTestNoLock`] preserves the call-site shape
//! (`let _lock = lock_harn_process_tests();`) so the migration is local
//! to this support module rather than 30+ test sites.

/// Drop-in replacement for the old cross-process lock guard.
///
/// Holding this sentinel does nothing — it exists so that
/// `let _lock = lock_harn_process_tests();` keeps working while
/// the actual synchronization is removed. See module docs for rationale.
pub struct HarnProcessTestNoLock;

#[allow(dead_code)]
pub fn lock_harn_process_tests() -> HarnProcessTestNoLock {
    HarnProcessTestNoLock
}
