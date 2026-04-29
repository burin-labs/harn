use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

use super::{VmError, VmValue};

/// The raw join handle type for spawned tasks.
pub type VmJoinHandle = tokio::task::JoinHandle<Result<(VmValue, String), VmError>>;

/// A spawned async task handle with cancellation support.
pub struct VmTaskHandle {
    pub handle: VmJoinHandle,
    /// Cooperative cancellation token. Set to true to request graceful shutdown.
    pub cancel_token: Arc<AtomicBool>,
}

/// A channel handle for the VM (uses tokio mpsc).
#[derive(Debug, Clone)]
pub struct VmChannelHandle {
    pub name: Rc<str>,
    pub sender: Arc<tokio::sync::mpsc::Sender<VmValue>>,
    pub receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
    pub closed: Arc<AtomicBool>,
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
    pub done: Rc<std::cell::Cell<bool>>,
    /// Receiver end of the yield channel (generator sends values here).
    /// Wrapped in a shared async mutex so recv() can be called without holding
    /// a RefCell borrow across await points.
    pub receiver: Rc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Result<VmValue, VmError>>>>,
}

/// A stream object: lazily produces values from a `gen fn`.
#[derive(Debug, Clone)]
pub struct VmStream {
    /// Whether the stream has finished (returned, thrown, or exhausted).
    pub done: Rc<std::cell::Cell<bool>>,
    /// Receiver end of the stream channel.
    pub receiver: Rc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Result<VmValue, VmError>>>>,
}
