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
    pause_started: parking_lot::Mutex<Option<Instant>>,
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
            pause_started: parking_lot::Mutex::new(None),
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

    pub(crate) fn pause(self: &Arc<Self>) -> Option<ExecutionDeadlinePauseGuard> {
        if !self.is_active() {
            return None;
        }
        let mut started = self.pause_started.lock();
        let previous = self.pause_depth.fetch_add(1, Ordering::AcqRel);
        if previous == 0 {
            *started = Some(Instant::now());
            // `notify_one` retains a permit if the interpreter's select has
            // not polled its deadline branch yet; `notify_waiters` would lose
            // that edge and let the stale pre-pause timer fire.
            self.changed.notify_one();
        }
        drop(started);
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

impl Drop for ExecutionDeadlinePauseGuard {
    fn drop(&mut self) {
        let mut started = self.state.pause_started.lock();
        let depth = self.state.pause_depth.load(Ordering::Acquire);
        debug_assert!(depth > 0, "execution deadline pause depth underflow");
        if depth == 1 {
            let paused_for = started.take().map(|at| at.elapsed()).unwrap_or_default();
            let add = u64::try_from(paused_for.as_nanos()).unwrap_or(u64::MAX);
            let _ = self.state.deadline_offset.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |encoded| (encoded != 0).then(|| encoded.saturating_add(add)),
            );
        }
        self.state.pause_depth.fetch_sub(1, Ordering::AcqRel);
        drop(started);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn host_admission_pause_defers_only_the_outer_execution_deadline() {
        let now = Instant::now();
        let state = ExecutionDeadlineState::new(now, Some(now + Duration::from_secs(1)));
        let before = state.current().expect("deadline is active");

        let pause = state.pause().expect("active deadline can pause");
        assert_eq!(state.current(), None, "paused host work is not runnable");
        // Drive the clock-independent contract directly: the guard owns
        // elapsed admission accounting without scheduler sleeps in this test.
        *state.pause_started.lock() = Some(Instant::now() - Duration::from_millis(25));
        drop(pause);

        let after = state.current().expect("deadline resumes after admission");
        assert!(after.duration_since(before) >= Duration::from_millis(25));
    }
}
