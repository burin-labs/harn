use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use super::{VmError, VmValue};

/// The raw join handle type for spawned tasks.
pub type VmJoinHandle = tokio::task::JoinHandle<Result<(VmValue, String), VmError>>;

/// A spawned async task handle with cancellation support.
pub struct VmTaskHandle {
    pub handle: VmJoinHandle,
    /// Cooperative cancellation token. Set to true to request graceful shutdown.
    pub cancel_token: Arc<AtomicBool>,
    /// Runtime-context task id used by the VM scheduler and wait-for graph.
    pub wait_task_id: String,
}

/// A channel handle for the VM (uses tokio mpsc).
#[derive(Debug, Clone)]
pub struct VmChannelHandle {
    pub name: Arc<str>,
    pub sender: Arc<tokio::sync::mpsc::Sender<VmValue>>,
    pub receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
    pub close: Arc<VmChannelCloseState>,
}

#[derive(Debug)]
pub struct VmChannelCloseState {
    closed: AtomicBool,
    signal: tokio::sync::watch::Sender<bool>,
}

impl VmChannelCloseState {
    pub(crate) fn open() -> Self {
        let (signal, _) = tokio::sync::watch::channel(false);
        Self {
            closed: AtomicBool::new(false),
            signal,
        }
    }

    pub(crate) fn close(&self) -> bool {
        if self.closed.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.signal.send_replace(true);
        true
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.signal.subscribe()
    }
}

impl VmChannelHandle {
    pub(crate) fn close(&self) -> bool {
        self.close.close()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.close.is_closed()
    }

    pub(crate) fn subscribe_closed(&self) -> tokio::sync::watch::Receiver<bool> {
        self.close.subscribe()
    }
}

/// An atomic integer handle for the VM.
#[derive(Debug, Clone)]
pub struct VmAtomicHandle {
    pub value: Arc<AtomicI64>,
}

/// A reproducible random number generator handle.
#[derive(Clone)]
pub struct VmRngHandle {
    pub rng: Arc<Mutex<rand::rngs::StdRng>>,
}

impl std::fmt::Debug for VmRngHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VmRngHandle { .. }")
    }
}

/// A host-minted proof-of-execution receipt: the payload of a positive
/// `Verdict`. Constructed ONLY by the verdict issuance capability
/// (`harness.verdict.issue`) from the host-owned record of a REAL `run_test`
/// execution — resolved by its opaque `result_handle`, whose disposition the
/// host froze at execution time. Issuance reads no caller-supplied filesystem
/// bytes, so a caller can forge neither the receipt's TYPE (no literal syntax,
/// no public builtin hands back the bare handle) nor its PROVENANCE (an authored
/// file has no handle in the execution store). It is refused by the durable
/// serialization seams, so a positive verdict cannot be minted, forged, or
/// replayed by asserting scalars or fabricating evidence. The content hash + run
/// identity close the tamper and cross-run-replay classes: the hash fingerprints
/// the bytes the host captured, and consumers reject a receipt whose
/// `execution_scope` differs from the active run.
#[derive(Debug, Clone)]
pub struct VmVerdictReceipt {
    /// Stable identity of the attested execution — the `run_test` `result_handle`
    /// the host recorded the run under.
    pub artifact_id: Arc<str>,
    /// `sha256:HEX` of the output the host captured from the real execution,
    /// snapshotted when the run was recorded.
    pub content_hash: Arc<str>,
    /// Passing and total checked-unit counts the host COMPUTED from the real
    /// execution, never a caller scalar. `passed > 0` is required to mint.
    pub passed: u32,
    pub total: u32,
    /// The execution scope that PRODUCED the evidence — captured at `run_test`
    /// record time, not at receipt-mint time. `verdict_all` rejects receipts
    /// whose `execution_scope` differ (cross-run replay) AND requires the active
    /// scope to still equal it, so a receipt cannot be replayed into a later run.
    pub execution_scope: Arc<str>,
    /// Optional subject identity (which unit-of-work the evidence attests). Folded
    /// in when the artifact carries it; when absent it is a NAMED limit (PR body).
    pub subject: Option<Arc<str>>,
}

/// A held synchronization permit for mutex/semaphore/gate primitives.
#[derive(Debug, Clone)]
pub struct VmSyncPermitHandle {
    pub(crate) lease: Arc<crate::synchronization::VmSyncLease>,
}

impl VmSyncPermitHandle {
    pub(crate) fn release(&self) -> bool {
        self.lease.release()
    }

    pub(crate) fn kind(&self) -> &str {
        self.lease.kind()
    }

    pub(crate) fn key(&self) -> &str {
        self.lease.key()
    }

    pub(crate) fn permits(&self) -> u32 {
        self.lease.permits()
    }

    pub(crate) fn is_released(&self) -> bool {
        self.lease.is_released()
    }

    pub(crate) fn same_lease(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
    }
}

/// A lazy integer range — Python-style. Stores only `(start, end, inclusive)`
/// so the in-memory footprint is O(1) regardless of the range's length.
/// `len()`, indexing (`r[k]`), `.contains(x)`, `.first()`, `.last()` are all
/// O(1); direct iteration walks step-by-step without materializing a list.
///
/// Empty-range convention (Python-consistent):
/// - Inclusive empty when `start > end`.
/// - Exclusive empty when `start >= end`.
///
/// Negative / reversed ranges are NOT supported in v1: `5 to 1` is simply
/// empty. Authors who want reverse iteration should call `.to_list().reverse()`.
#[derive(Debug, Clone, Copy)]
pub struct VmRange {
    pub start: i64,
    pub end: i64,
    pub inclusive: bool,
}

impl VmRange {
    /// Number of elements this range yields.
    ///
    /// Uses saturating arithmetic so that pathological ranges near
    /// `i64::MAX`/`i64::MIN` do not panic on overflow. Because a range's
    /// element count must fit in `i64` the returned length saturates at
    /// `i64::MAX` for ranges whose width exceeds that (e.g. `i64::MIN to
    /// i64::MAX` inclusive). Callers that later narrow to `usize` for
    /// allocation should still guard against huge lengths — see
    /// `to_vec` / `get` for the indexable-range invariants.
    pub fn len(&self) -> i64 {
        if self.inclusive {
            if self.start > self.end {
                0
            } else {
                self.end.saturating_sub(self.start).saturating_add(1)
            }
        } else if self.start >= self.end {
            0
        } else {
            self.end.saturating_sub(self.start)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element at the given 0-based index, bounds-checked.
    /// Returns `None` when out of bounds or when `start + idx` would
    /// overflow (which can only happen when `len()` saturated).
    pub fn get(&self, idx: i64) -> Option<i64> {
        if idx < 0 || idx >= self.len() {
            None
        } else {
            self.start.checked_add(idx)
        }
    }

    /// First element or `None` when empty.
    pub fn first(&self) -> Option<i64> {
        if self.is_empty() {
            None
        } else {
            Some(self.start)
        }
    }

    /// Last element or `None` when empty.
    pub fn last(&self) -> Option<i64> {
        if self.is_empty() {
            None
        } else if self.inclusive {
            Some(self.end)
        } else {
            Some(self.end - 1)
        }
    }

    /// Whether `v` falls inside the range (O(1)).
    pub fn contains(&self, v: i64) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.inclusive {
            v >= self.start && v <= self.end
        } else {
            v >= self.start && v < self.end
        }
    }

    /// Materialize to a `Vec<VmValue>` — the explicit escape hatch.
    ///
    /// Uses `checked_add` on the per-element index so a range near
    /// `i64::MAX` stops at the representable bound instead of panicking.
    /// Callers should still treat a very long range as unwise to
    /// materialize (the whole point of `VmRange` is to avoid this).
    pub fn to_vec(&self) -> Vec<VmValue> {
        let len = self.len();
        if len <= 0 {
            return Vec::new();
        }
        let cap = len as usize;
        let mut out = Vec::with_capacity(cap);
        for i in 0..len {
            match self.start.checked_add(i) {
                Some(v) => out.push(VmValue::Int(v)),
                None => break,
            }
        }
        out
    }
}

/// A generator object: lazily produces values via yield.
/// The generator body runs as a spawned task that sends values through a channel.
#[derive(Debug, Clone)]
pub struct VmGenerator {
    /// Whether the generator has finished (returned or exhausted).
    pub done: Arc<AtomicBool>,
    /// Receiver end of the yield channel (generator sends values here).
    /// Wrapped in a shared async mutex so recv() can be called without holding
    /// a synchronous iterator-state lock across await points.
    pub receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Result<VmValue, VmError>>>>,
}

impl VmGenerator {
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_done(&self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// A stream object: lazily produces values from a `gen fn`.
#[derive(Debug, Clone)]
pub struct VmStream {
    /// Whether the stream has finished (returned, thrown, or exhausted).
    pub done: Arc<AtomicBool>,
    /// Receiver end of the stream channel.
    pub receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Result<VmValue, VmError>>>>,
    /// Optional cancellation hook for host-backed streams.
    pub cancel: Option<VmStreamCancel>,
}

impl VmStream {
    pub(crate) fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_done(&self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct VmStreamCancel {
    sender: Arc<tokio::sync::watch::Sender<bool>>,
}

impl VmStreamCancel {
    pub fn new() -> Self {
        let (sender, _receiver) = tokio::sync::watch::channel(false);
        Self {
            sender: Arc::new(sender),
        }
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.sender.subscribe()
    }
}

impl Default for VmStreamCancel {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for VmStreamCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmStreamCancel")
            .field("cancelled", &*self.sender.borrow())
            .finish()
    }
}

impl VmStream {
    pub(crate) fn cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}
