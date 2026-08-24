use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Cancellation-safe host deadline state. The guard owns the shared state so
/// dropping an in-flight `execute_with_timeout` future restores the previous
/// deadline and poisons the abandoned VM even though the future still holds
/// `&mut Vm`.
pub(crate) struct ExecutionDeadlineState {
    origin: Instant,
    /// Nanoseconds from `origin`, plus one so zero remains the inactive value.
    deadline_offset: AtomicU64,
    pause_depth: AtomicU64,
    pause_window: parking_lot::Mutex<Option<PauseWindow>>,
    changed: tokio::sync::Notify,
    /// Set when the host drops a polled execution future. Arbitrary async
    /// cancellation cannot unwind interpreter state, so subsequent execution
    /// entries fail loudly instead of resuming a partial frame.
    abandoned: AtomicBool,
}

impl ExecutionDeadlineState {
    pub(crate) fn new(origin: Instant, deadline: Option<Instant>) -> Arc<Self> {
        Arc::new(Self {
            origin,
            deadline_offset: AtomicU64::new(Self::encode(origin, deadline)),
            pause_depth: AtomicU64::new(0),
            pause_window: parking_lot::Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            abandoned: AtomicBool::new(false),
        })
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        self.deadline_offset.load(Ordering::Acquire) != 0
    }

    #[inline]
    pub(crate) fn is_abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }

    pub(crate) fn fork(&self) -> Arc<Self> {
        let state = Self::new(self.origin, self.raw_current());
        state
            .abandoned
            .store(self.is_abandoned(), Ordering::Release);
        state
    }

    pub(crate) fn current(&self) -> Option<Instant> {
        if self.pause_depth.load(Ordering::Acquire) != 0 {
            return None;
        }
        self.raw_current()
    }

    fn raw_current(&self) -> Option<Instant> {
        let encoded = self.deadline_offset.load(Ordering::Acquire);
        (encoded != 0)
            .then(|| self.origin + std::time::Duration::from_nanos(encoded.saturating_sub(1)))
    }

    #[cfg(test)]
    pub(crate) fn encoded_offset_for_test(&self) -> u64 {
        self.deadline_offset.load(Ordering::Acquire)
    }

    pub(crate) fn pause(
        self: &Arc<Self>,
        clock: Arc<dyn harn_clock::Clock>,
    ) -> Option<ExecutionDeadlinePauseGuard> {
        if !self.is_active() {
            return None;
        }
        let mut window = self.pause_window.lock();
        let previous = self.pause_depth.fetch_add(1, Ordering::AcqRel);
        if previous == 0 {
            *window = Some(PauseWindow {
                started_ms: clock.monotonic_ms(),
                clock,
            });
            // `notify_one` retains a permit if the interpreter's select has
            // not polled its deadline branch yet; `notify_waiters` would lose
            // that edge and let the stale pre-pause timer fire.
            self.changed.notify_one();
        }
        drop(window);
        Some(ExecutionDeadlinePauseGuard {
            state: Arc::clone(self),
        })
    }

    pub(crate) fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }

    pub(crate) fn install(self: &Arc<Self>, deadline: Instant) -> ExecutionDeadlineGuard {
        let previous = self.deadline_offset.load(Ordering::Acquire);
        let requested = Self::encode(self.origin, Some(deadline));
        let active = if previous == 0 {
            requested
        } else {
            previous.min(requested)
        };
        self.deadline_offset.store(active, Ordering::Release);
        ExecutionDeadlineGuard {
            state: Arc::clone(self),
            previous,
            completed: false,
        }
    }

    fn encode(origin: Instant, deadline: Option<Instant>) -> u64 {
        deadline.map_or(0, |deadline| {
            let nanos = deadline.saturating_duration_since(origin).as_nanos();
            u64::try_from(nanos)
                .unwrap_or(u64::MAX - 1)
                .saturating_add(1)
        })
    }
}

pub(crate) struct ExecutionDeadlinePauseGuard {
    state: Arc<ExecutionDeadlineState>,
}

struct PauseWindow {
    started_ms: i64,
    clock: Arc<dyn harn_clock::Clock>,
}

impl Drop for ExecutionDeadlinePauseGuard {
    fn drop(&mut self) {
        let mut window = self.state.pause_window.lock();
        let depth = self.state.pause_depth.load(Ordering::Acquire);
        debug_assert!(depth > 0, "execution deadline pause depth underflow");
        if depth == 1 {
            let paused_for_ms = window
                .take()
                .map(|window| {
                    window
                        .clock
                        .monotonic_ms()
                        .saturating_sub(window.started_ms)
                })
                .unwrap_or_default();
            let add = u64::try_from(paused_for_ms)
                .unwrap_or_default()
                .saturating_mul(1_000_000);
            let _ = self.state.deadline_offset.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |encoded| (encoded != 0).then(|| encoded.saturating_add(add)),
            );
        }
        self.state.pause_depth.fetch_sub(1, Ordering::AcqRel);
        drop(window);
        self.state.changed.notify_one();
    }
}

pub(crate) struct ExecutionDeadlineGuard {
    state: Arc<ExecutionDeadlineState>,
    previous: u64,
    completed: bool,
}

impl ExecutionDeadlineGuard {
    /// Mark an awaited execution as terminal before restoring its prior host
    /// deadline. Dropping without this acknowledgement poisons the VM.
    pub(crate) fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for ExecutionDeadlineGuard {
    fn drop(&mut self) {
        self.state
            .deadline_offset
            .store(self.previous, Ordering::Release);
        if !self.completed {
            self.state.abandoned.store(true, Ordering::Release);
        }
    }
}
