//! Per-builtin wall-time attribution.
//!
//! The categorical [`crate::profile`] rollup answers "LLM vs tools vs steps",
//! which is the right question for an agent run and the wrong one for a script
//! that is simply slow: every builtin a pipeline calls — a project scan, a
//! subprocess, a file read — lands in `vm/residual` alongside the script's own
//! bytecode. A run that spends ten seconds inside one builtin reports
//! `vm/residual 100.0%`, which names nothing.
//!
//! Builtins are called far too often to afford a [`crate::tracing::Span`] each
//! (a string name, a metadata map, a vector push). This records two integers per
//! builtin NAME instead — call count and total nanoseconds — behind an atomic
//! that is only set when the operator asked for a profile. When it is off, the
//! cost is one relaxed atomic load per builtin call.
//!
//! Two limits, stated so the table is not read for more than it says:
//!
//! Time is INCLUSIVE. A builtin that calls back into the VM (and so into other
//! builtins) counts its callees' time as well as its own. That is the honest
//! reading for the question this answers — "what is this run waiting on" — but
//! it means the column does not sum to wall time, and the renderer says so.
//!
//! Each timed call carries its own measurement cost (a clock read and a lock),
//! charged to the builtin being measured. That is noise against a builtin that
//! takes ten seconds and a meaningful fraction of one that takes a microsecond,
//! so this finds expensive builtins — it is not a microbenchmark of cheap ones.
//! Rank the table; do not read a cheap builtin's per-call average as its true
//! cost.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Whether to record. Read once per builtin call, so it stays an atomic rather
/// than a lock.
static ENABLED: AtomicBool = AtomicBool::new(false);

static TOTALS: Mutex<Option<HashMap<String, BuiltinTotal>>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, Default)]
struct BuiltinTotal {
    calls: u64,
    nanos: u128,
}

/// One builtin's aggregated cost across a run.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BuiltinBucket {
    pub name: String,
    pub total_ms: u64,
    pub calls: u64,
    /// Mean call cost. The interesting split is one slow call versus a million
    /// fast ones, and a total alone cannot tell them apart.
    pub avg_ms: f64,
}

/// Start recording, discarding anything from a previous run.
///
/// The recorder is enabled per RUN but lives for the PROCESS, so the returned
/// guard ends the recording when the run that asked for it finishes. Without
/// it an embedder that profiles one script and not the next would keep paying
/// for bookkeeping nobody reads, and fold the second run's builtins into the
/// first run's totals.
///
/// The lifecycle belongs to the caller and nowhere else. It used to also end
/// at `reset_thread_local_state`, which runs from ~150 test setups and from
/// production entry points like `execute_conformance_source`; any of them
/// firing mid-run disarmed the recorder and the profile named nothing.
#[must_use = "recording ends when the returned guard drops"]
pub fn enable() -> RecordingGuard {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    *TOTALS.lock().unwrap_or_else(|e| e.into_inner()) = Some(HashMap::new());
    ENABLED.store(true, Ordering::Relaxed);
    RecordingGuard { generation }
}

/// Which `enable()` currently owns the recorder.
///
/// The recorder is one process-global, so two overlapping profiled runs cannot
/// both have their own. The generation makes the overlap degrade predictably
/// instead of silently: the later `enable()` takes ownership, and the earlier
/// guard's drop becomes a no-op rather than disarming a run that is still
/// going. Whoever holds the current generation ends the recording.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Ends builtin recording when dropped, unless a later [`enable`] has since
/// taken ownership of the recorder.
pub struct RecordingGuard {
    generation: u64,
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        if GENERATION.load(Ordering::SeqCst) == self.generation {
            reset();
        }
    }
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Record one completed builtin call.
///
/// Allocates only the first time a given builtin is seen. `entry()` would take
/// an owned key and so allocate on every call — and the cost of measuring a
/// builtin is charged to that builtin, so a per-call allocation shows up as the
/// hot spot instead of finding it. `len` called 200k times reported a second of
/// wall time that was almost entirely this function.
pub fn record(name: &str, elapsed: Duration) {
    if !is_enabled() {
        return;
    }
    let mut guard = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(totals) = guard.as_mut() else {
        return;
    };
    let nanos = elapsed.as_nanos();
    if let Some(entry) = totals.get_mut(name) {
        entry.calls += 1;
        entry.nanos += nanos;
        return;
    }
    totals.insert(name.to_string(), BuiltinTotal { calls: 1, nanos });
}

/// Times one builtin call and records it on drop.
///
/// Scope-guard shaped, like [`crate::vm::ScopeSpan`], so a dispatch path adopts
/// it with a binding rather than a matched pair of calls it could return early
/// between. A builtin that throws still reports the time it burned first.
///
/// Borrows the name rather than owning it: a dispatch path always has the name
/// alive across the call it is timing, and copying it per call would charge the
/// copy to the builtin being measured.
pub struct BuiltinTimer<'a> {
    name: &'a str,
    start: std::time::Instant,
}

impl<'a> BuiltinTimer<'a> {
    /// `None` when profiling is off, which costs one relaxed atomic load and
    /// keeps `Instant::now` off the hot path of every `to_string` call.
    pub fn start(name: &'a str) -> Option<Self> {
        if !is_enabled() {
            return None;
        }
        Some(Self {
            name,
            start: std::time::Instant::now(),
        })
    }
}

impl Drop for BuiltinTimer<'_> {
    fn drop(&mut self) {
        record(self.name, self.start.elapsed());
    }
}

/// The recorded buckets, most expensive first. Empty when profiling was off.
pub fn snapshot() -> Vec<BuiltinBucket> {
    let guard = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(totals) = guard.as_ref() else {
        return Vec::new();
    };
    let mut buckets: Vec<BuiltinBucket> = totals
        .iter()
        .map(|(name, total)| {
            let total_ms = (total.nanos / 1_000_000) as u64;
            BuiltinBucket {
                name: name.clone(),
                total_ms,
                calls: total.calls,
                avg_ms: if total.calls == 0 {
                    0.0
                } else {
                    (total.nanos as f64 / total.calls as f64) / 1_000_000.0
                },
            }
        })
        .collect();
    // Ties broken by name so the rendered table is stable across runs.
    buckets.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    buckets
}

/// Stop recording and drop the totals.
pub fn reset() {
    ENABLED.store(false, Ordering::Relaxed);
    *TOTALS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The one lock serializing tests that touch the process-global recorder.
///
/// Lives outside `mod tests` because the lifecycle regression for this module
/// sits next to `reset_thread_local_state` — it asserts that a global reset
/// does NOT disarm an in-flight profiled run — and both sides must hold the
/// SAME lock or they race each other's `enable`/`reset` under cargo's test
/// threads.
#[cfg(test)]
pub(crate) fn test_lock() -> &'static Mutex<()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    &TEST_LOCK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The global recorder is process-wide, so these run under one lock rather
    /// than racing each other through `enable`/`reset`.
    use super::test_lock as TEST_LOCK;

    #[test]
    fn records_nothing_until_enabled() {
        let _guard = TEST_LOCK().lock().unwrap_or_else(|e| e.into_inner());
        reset();
        record("project_fingerprint", Duration::from_millis(10));
        assert!(
            snapshot().is_empty(),
            "a run without --profile must not pay for bookkeeping it never reads"
        );
    }

    #[test]
    fn aggregates_calls_by_name_and_ranks_by_total() {
        let _guard = TEST_LOCK().lock().unwrap_or_else(|e| e.into_inner());
        let _recording = enable();
        record("to_string", Duration::from_millis(1));
        record("to_string", Duration::from_millis(3));
        record("project_fingerprint", Duration::from_millis(120));
        let buckets = snapshot();
        reset();

        assert_eq!(buckets.len(), 2);
        // One slow call outranks several fast ones: the ranking is by cost, not
        // by how busy a builtin looks.
        assert_eq!(buckets[0].name, "project_fingerprint");
        assert_eq!(buckets[0].total_ms, 120);
        assert_eq!(buckets[0].calls, 1);
        assert_eq!(buckets[1].name, "to_string");
        assert_eq!(buckets[1].total_ms, 4);
        assert_eq!(buckets[1].calls, 2);
        assert!((buckets[1].avg_ms - 2.0).abs() < 0.001);
    }

    #[test]
    fn enable_discards_a_previous_run() {
        let _guard = TEST_LOCK().lock().unwrap_or_else(|e| e.into_inner());
        let _recording = enable();
        record("run_shell", Duration::from_millis(5));
        let _recording = enable();
        let buckets = snapshot();
        reset();
        assert!(
            buckets.is_empty(),
            "a fresh run must not inherit stale totals"
        );
    }

    /// Two profiled runs can overlap in one process — an embedder driving two
    /// VMs, or a `--profile` run that starts while another is finishing. There
    /// is only one recorder, so the later run owns it; the earlier run's guard
    /// must not disarm it on the way out.
    #[test]
    fn an_outgoing_run_does_not_disarm_the_run_that_replaced_it() {
        let _guard = TEST_LOCK().lock().unwrap_or_else(|e| e.into_inner());
        let first = enable();
        let second = enable();

        drop(first);

        assert!(
            is_enabled(),
            "the superseded run's guard must not stop the recorder"
        );
        record("run_shell", Duration::from_millis(5));
        assert!(
            !snapshot().is_empty(),
            "the owning run must still be recording"
        );

        drop(second);
        assert!(!is_enabled(), "the owning run's guard ends the recording");
    }
}
