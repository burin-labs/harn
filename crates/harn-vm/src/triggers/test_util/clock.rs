//! Single source of truth for clock mocking across the VM, built on the
//! unified [`harn_clock::Clock`] trait.
//!
//! This module owns the thread-local "mock clock" stack consulted by
//! every time-sensitive subsystem: stdlib `now_ms` / `monotonic_ms` /
//! `sleep` builtins, the trigger dispatcher, the cron scheduler, and any
//! Rust-side test that needs to pin or advance time. All of them route
//! through [`active_mock_clock`] so that pushing one mock pins time
//! everywhere it would otherwise be read.
//!
//! The lives-in-`triggers/` path is historical — the abstraction is now
//! crate-wide and re-exported as `harn_vm::clock_mock` for callers
//! outside the trigger subsystem. New runtime code that needs an
//! injectable clock should accept `Arc<dyn harn_clock::Clock>` directly.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use async_trait::async_trait;
use harn_clock::Clock;
use time::OffsetDateTime;

thread_local! {
    static MOCK_CLOCK_STACK: RefCell<Vec<Arc<MockClock>>> = const { RefCell::new(Vec::new()) };
}

fn process_start() -> &'static Instant {
    static PROCESS_START: OnceLock<Instant> = OnceLock::new();
    PROCESS_START.get_or_init(Instant::now)
}

/// Monotonic instant snapshot with millisecond resolution. Compares cheaply,
/// serialises by value, and abstracts whether it came from a paused clock or
/// the real OS monotonic source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockInstant(StdDuration);

impl ClockInstant {
    pub fn duration_since(self, earlier: Self) -> StdDuration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn as_millis(self) -> u128 {
        self.0.as_millis()
    }
}

pub struct ClockOverrideGuard;

impl Drop for ClockOverrideGuard {
    fn drop(&mut self) {
        // `try_with` handles the case where TLS is already being torn down
        // at process exit, which causes a panic if accessed via `with`.
        let _ = MOCK_CLOCK_STACK.try_with(|slot| {
            slot.borrow_mut().pop();
        });
    }
}

/// Test-facing wrapper around [`harn_clock::PausedClock`].
///
/// Provides the sync ergonomics (`set_sync`, `advance_std_sync`) that
/// stdlib builtins call without a runtime, plus async aliases (`set`,
/// `advance`, `advance_std`, `advance_ticks`) that the trigger/cron tests
/// were written against. Implements [`harn_clock::Clock`] so it can also
/// be handed directly to consumers that take `Arc<dyn Clock>`.
#[derive(Debug)]
pub struct MockClock {
    inner: Arc<harn_clock::PausedClock>,
}

impl MockClock {
    pub fn new(now: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self {
            inner: harn_clock::PausedClock::new(now),
        })
    }

    /// Construct a clock pinned to `wall_ms` (UNIX epoch milliseconds).
    pub fn at_wall_ms(wall_ms: i64) -> Arc<Self> {
        let nanos = (wall_ms as i128).saturating_mul(1_000_000);
        let now =
            OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH);
        Self::new(now)
    }

    pub fn monotonic_now(&self) -> ClockInstant {
        ClockInstant(StdDuration::from_millis(
            self.inner.monotonic_ms().max(0) as u64
        ))
    }

    /// Wall clock in millis since `UNIX_EPOCH`.
    pub fn now_wall_ms(&self) -> i64 {
        harn_clock::offset_datetime_to_ms(self.now_utc())
    }

    /// Monotonic millis since this clock was created.
    pub fn now_monotonic_ms(&self) -> i64 {
        self.inner.monotonic_ms()
    }

    /// Pin the wall clock to `now` (sync). Sleepers waiting for a deadline
    /// that has now passed are released.
    pub fn set_sync(&self, now: OffsetDateTime) {
        self.inner.set(now);
    }

    /// Advance the clock by `duration` (sync).
    pub fn advance_std_sync(&self, duration: StdDuration) {
        self.inner.advance(duration);
    }

    /// Async alias for [`set_sync`]. Kept so existing trigger/cron tests
    /// (which were written under the old async API) continue to compile.
    pub async fn set(&self, now: OffsetDateTime) {
        self.inner.set(now);
    }

    /// Advance by a `time::Duration`. Negative values are clamped to zero.
    pub async fn advance(&self, duration: time::Duration) {
        self.inner.advance_time(duration);
    }

    /// Advance by a `std::time::Duration`.
    pub async fn advance_std(&self, duration: StdDuration) {
        self.inner.advance(duration);
    }

    /// Step `ticks` times by `tick`, notifying sleepers between every step.
    pub async fn advance_ticks(&self, ticks: u32, tick: StdDuration) {
        self.inner.advance_ticks(ticks, tick);
    }
}

#[async_trait]
impl Clock for MockClock {
    fn now_utc(&self) -> OffsetDateTime {
        self.inner.now_utc()
    }

    fn monotonic_ms(&self) -> i64 {
        self.inner.monotonic_ms()
    }

    async fn sleep(&self, duration: StdDuration) {
        self.inner.sleep(duration).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        self.inner.sleep_until_utc(deadline).await;
    }
}

pub fn install_override(clock: Arc<MockClock>) -> ClockOverrideGuard {
    MOCK_CLOCK_STACK.with(|slot| {
        slot.borrow_mut().push(clock);
    });
    ClockOverrideGuard
}

pub fn active_mock_clock() -> Option<Arc<MockClock>> {
    MOCK_CLOCK_STACK.with(|slot| slot.borrow().last().cloned())
}

pub fn is_mocked() -> bool {
    MOCK_CLOCK_STACK.with(|slot| !slot.borrow().is_empty())
}

/// Clear the entire override stack. Called from the per-test reset hook
/// so stray overrides (e.g. from a Harn pipeline that called
/// `mock_time` without `unmock_time`) cannot leak between tests.
pub fn clear_overrides() {
    MOCK_CLOCK_STACK.with(|slot| {
        slot.borrow_mut().clear();
    });
}

pub fn now_utc() -> OffsetDateTime {
    active_mock_clock()
        .map(|clock| clock.now_utc())
        .unwrap_or_else(OffsetDateTime::now_utc)
}

pub fn now_ms() -> i64 {
    harn_clock::offset_datetime_to_ms(now_utc())
}

pub fn instant_now() -> ClockInstant {
    active_mock_clock()
        .map(|clock| clock.monotonic_now())
        .unwrap_or_else(|| ClockInstant(process_start().elapsed()))
}

/// Advance the topmost mock clock by `duration`. No-op if no clock is
/// installed.
pub fn advance(duration: StdDuration) {
    if let Some(clock) = active_mock_clock() {
        clock.advance_std_sync(duration);
    }
}

/// Sleep for `duration`, honoring the unified mock clock.
///
/// When a `MockClock` override is installed the sleep advances the mock
/// instantly and returns — the same semantics scripts get from
/// `mock_time(...)` + `sleep(...)`. When no mock is installed the call
/// falls through to `tokio::time::sleep`, which is itself virtualized
/// inside a `#[tokio::test(start_paused = true)]` runtime.
///
/// Production runtime code that needs to back off (rate limiters,
/// retry loops, the trigger dispatcher's flow-control window) should
/// route through this helper so a single Rust test can pin time across
/// both Harn and Rust layers.
pub async fn sleep(duration: StdDuration) {
    if duration.is_zero() {
        return;
    }
    if let Some(mock) = active_mock_clock() {
        mock.sleep(duration).await;
        return;
    }
    tokio::time::sleep(duration).await;
}
