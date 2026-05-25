//! Unified `Clock` trait for the Harn runtime.
//!
//! Production code that needs the current time, monotonic measurement, or a
//! cancellable sleep takes an `Arc<dyn Clock>` and reads through it. Real
//! deployments wire [`RealClock`]; tests substitute [`PausedClock`] to drive
//! virtual time deterministically; recording layers wrap any inner clock with
//! [`RecordedClock`] to capture every observation for replay.
//!
//! See `docs/src/dev/testing.md` for usage patterns.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Notify;

/// Unified clock abstraction.
///
/// All time observations and sleeps in production-facing Harn code should
/// route through a `Clock`. Cron, the trigger dispatcher, the stdlib
/// `now_ms` / `sleep_ms` builtins, and the `OrchestratorHarness` all accept
/// `Arc<dyn Clock>` so test harnesses can swap in [`PausedClock`].
#[async_trait]
pub trait Clock: Send + Sync + fmt::Debug {
    /// Current wall-clock UTC time.
    fn now_utc(&self) -> OffsetDateTime;

    /// Monotonic milliseconds since an implementation-defined origin.
    ///
    /// Required to be non-decreasing across calls on the same `Clock`
    /// instance. Used by the stdlib `monotonic_ms` / `elapsed` builtins and
    /// any code measuring durations across operations.
    fn monotonic_ms(&self) -> i64;

    /// Sleep for `duration`. No-op when `duration` is zero.
    async fn sleep(&self, duration: Duration);

    /// Sleep until the wall-clock UTC `deadline`. No-op if `deadline` is in
    /// the past.
    async fn sleep_until_utc(&self, deadline: OffsetDateTime);
}

/// Convert an `OffsetDateTime` to Unix epoch milliseconds. The division
/// happens in `i128` so timestamps past April 2262 — where nanoseconds
/// exceed `i64::MAX` — don't silently corrupt when cast.
pub fn offset_datetime_to_ms(ts: OffsetDateTime) -> i64 {
    (ts.unix_timestamp_nanos() / 1_000_000) as i64
}

/// Convenience: current wall-clock millis since UNIX_EPOCH.
pub fn now_wall_ms(clock: &dyn Clock) -> i64 {
    offset_datetime_to_ms(clock.now_utc())
}

// ── Real clock ─────────────────────────────────────────────────────────────────

/// Production clock. Reads `OffsetDateTime::now_utc()` and
/// `tokio::time::sleep`. Honors `tokio::time::pause()` for sleep-driven
/// scheduling but `now_utc` always returns true wall time.
pub struct RealClock {
    monotonic_origin: tokio::time::Instant,
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for RealClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealClock").finish()
    }
}

impl RealClock {
    pub fn new() -> Self {
        Self {
            monotonic_origin: tokio::time::Instant::now(),
        }
    }

    /// Convenience: wrap in an `Arc` for handing to consumers.
    pub fn arc() -> Arc<dyn Clock> {
        Arc::new(Self::new())
    }
}

#[async_trait]
impl Clock for RealClock {
    fn now_utc(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn monotonic_ms(&self) -> i64 {
        let elapsed = tokio::time::Instant::now().saturating_duration_since(self.monotonic_origin);
        elapsed.as_millis() as i64
    }

    async fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        tokio::time::sleep(duration).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        let now = self.now_utc();
        if deadline <= now {
            return;
        }
        let delta = deadline - now;
        let Ok(duration) = Duration::try_from(delta) else {
            return;
        };
        tokio::time::sleep(duration).await;
    }
}

// ── Paused clock ───────────────────────────────────────────────────────────────

/// Fully-virtual clock for tests.
///
/// Stores its own wall-clock cursor and a monotonic counter. Sleeps suspend on
/// an internal `Notify` and wake when [`PausedClock::advance`] or
/// [`PausedClock::set`] crosses the deadline. No tokio-runtime cooperation is
/// required: `PausedClock` works inside `current_thread`, `multi_thread`, and
/// `start_paused` runtimes equally well.
///
/// Pairs with `tokio::time::pause()` for tests that mix this clock with code
/// that uses `tokio::time::sleep` directly (e.g. `tokio::time::timeout`).
pub struct PausedClock {
    state: Mutex<PausedState>,
    notify: Notify,
}

struct PausedState {
    wall: OffsetDateTime,
    monotonic: Duration,
}

impl fmt::Debug for PausedClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        f.debug_struct("PausedClock")
            .field("wall", &state.wall)
            .field("monotonic_ms", &state.monotonic.as_millis())
            .finish()
    }
}

impl PausedClock {
    /// Build a paused clock pinned at `origin`.
    pub fn new(origin: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PausedState {
                wall: origin,
                monotonic: Duration::ZERO,
            }),
            notify: Notify::new(),
        })
    }

    /// Advance wall + monotonic by `duration` and wake any sleepers whose
    /// deadlines are now in the past.
    pub fn advance(&self, duration: Duration) {
        let delta = match time::Duration::try_from(duration) {
            Ok(value) => value,
            Err(_) => return,
        };
        {
            let mut state = self.state.lock();
            state.wall += delta;
            state.monotonic = state.monotonic.saturating_add(duration);
        }
        self.notify.notify_waiters();
    }

    /// Advance using a `time::Duration`. Negative durations are clamped to zero.
    pub fn advance_time(&self, duration: time::Duration) {
        let Ok(positive) = Duration::try_from(duration) else {
            return;
        };
        self.advance(positive);
    }

    /// Step `ticks` times by `tick`, notifying sleepers between every step so
    /// tasks observing intermediate wake-ups see each notification.
    pub fn advance_ticks(&self, ticks: u32, tick: Duration) {
        for _ in 0..ticks {
            self.advance(tick);
        }
    }

    /// Pin the wall clock to `wall`. Monotonic counter advances by the
    /// (signed) delta, never moving backwards.
    pub fn set(&self, wall: OffsetDateTime) {
        {
            let mut state = self.state.lock();
            let delta = wall - state.wall;
            state.wall = wall;
            if delta.is_positive() {
                if let Ok(positive) = Duration::try_from(delta) {
                    state.monotonic = state.monotonic.saturating_add(positive);
                }
            }
        }
        self.notify.notify_waiters();
    }
}

#[async_trait]
impl Clock for PausedClock {
    fn now_utc(&self) -> OffsetDateTime {
        self.state.lock().wall
    }

    fn monotonic_ms(&self) -> i64 {
        self.state.lock().monotonic.as_millis() as i64
    }

    async fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let deadline = self.state.lock().wall
            + time::Duration::try_from(duration).unwrap_or(time::Duration::ZERO);
        self.sleep_until_utc(deadline).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        loop {
            let notified = self.notify.notified();
            if self.state.lock().wall >= deadline {
                return;
            }
            notified.await;
        }
    }
}

// ── Recorded clock ─────────────────────────────────────────────────────────────

/// Single observation captured by [`RecordedClock`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClockEvent {
    NowUtc { wall_ns: i128 },
    MonotonicMs { value: i64 },
    Sleep { duration_ms: u64 },
    SleepUntil { wall_ns: i128 },
}

/// In-memory append-only log of clock observations.
#[derive(Debug, Default)]
pub struct ClockEventLog {
    events: Mutex<Vec<ClockEvent>>,
}

impl ClockEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an event. Called from `RecordedClock`; tests can also seed
    /// expected events when building a replay oracle.
    pub fn push(&self, event: ClockEvent) {
        self.events.lock().push(event);
    }

    /// Snapshot of events recorded so far.
    pub fn snapshot(&self) -> Vec<ClockEvent> {
        self.events.lock().clone()
    }

    /// Number of events recorded.
    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Wraps an inner [`Clock`] and records every observation to a
/// [`ClockEventLog`].
///
/// The recording is the substrate the testbench replay/recording feature
/// (#1441) builds on. It is deliberately scoped to the clock surface; other
/// I/O substrates record separately.
pub struct RecordedClock {
    inner: Arc<dyn Clock>,
    log: Arc<ClockEventLog>,
}

impl fmt::Debug for RecordedClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordedClock")
            .field("inner", &self.inner)
            .field("events", &self.log.len())
            .finish()
    }
}

impl RecordedClock {
    pub fn new(inner: Arc<dyn Clock>, log: Arc<ClockEventLog>) -> Self {
        Self { inner, log }
    }

    pub fn log(&self) -> Arc<ClockEventLog> {
        self.log.clone()
    }
}

#[async_trait]
impl Clock for RecordedClock {
    fn now_utc(&self) -> OffsetDateTime {
        let value = self.inner.now_utc();
        self.log.push(ClockEvent::NowUtc {
            wall_ns: value.unix_timestamp_nanos(),
        });
        value
    }

    fn monotonic_ms(&self) -> i64 {
        let value = self.inner.monotonic_ms();
        self.log.push(ClockEvent::MonotonicMs { value });
        value
    }

    async fn sleep(&self, duration: Duration) {
        self.log.push(ClockEvent::Sleep {
            duration_ms: duration.as_millis() as u64,
        });
        self.inner.sleep(duration).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        self.log.push(ClockEvent::SleepUntil {
            wall_ns: deadline.unix_timestamp_nanos(),
        });
        self.inner.sleep_until_utc(deadline).await;
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    }

    #[tokio::test]
    async fn real_clock_returns_increasing_monotonic() {
        let clock = RealClock::new();
        let a = clock.monotonic_ms();
        tokio::task::yield_now().await;
        let b = clock.monotonic_ms();
        assert!(b >= a, "monotonic must not move backwards");
    }

    #[tokio::test]
    async fn paused_clock_pins_wall_and_monotonic_until_advanced() {
        let clock = PausedClock::new(epoch());
        assert_eq!(clock.now_utc(), epoch());
        assert_eq!(clock.monotonic_ms(), 0);
        clock.advance(Duration::from_millis(250));
        assert_eq!(clock.monotonic_ms(), 250);
        assert_eq!(clock.now_utc(), epoch() + time::Duration::milliseconds(250));
    }

    #[tokio::test]
    async fn paused_clock_sleep_resumes_after_advance() {
        let clock = PausedClock::new(epoch());
        let clock_for_sleep = clock.clone();
        let task = tokio::spawn(async move {
            clock_for_sleep.sleep(Duration::from_secs(5)).await;
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "sleep should still be pending");
        clock.advance(Duration::from_secs(10));
        task.await.expect("sleep task panicked");
    }

    #[tokio::test]
    async fn paused_clock_sleep_until_returns_immediately_for_past_deadline() {
        let clock = PausedClock::new(epoch());
        clock.advance(Duration::from_mins(1));
        clock.sleep_until_utc(epoch()).await;
    }

    #[tokio::test]
    async fn recorded_clock_appends_one_event_per_call() {
        let log = Arc::new(ClockEventLog::new());
        let clock = RecordedClock::new(PausedClock::new(epoch()), log.clone());
        let _ = clock.now_utc();
        let _ = clock.monotonic_ms();
        clock.sleep(Duration::ZERO).await;
        clock.sleep_until_utc(epoch()).await;
        let events = log.snapshot();
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], ClockEvent::NowUtc { .. }));
        assert!(matches!(events[1], ClockEvent::MonotonicMs { .. }));
        assert!(matches!(events[2], ClockEvent::Sleep { duration_ms: 0 }));
        assert!(matches!(events[3], ClockEvent::SleepUntil { .. }));
    }

    #[tokio::test]
    async fn now_wall_ms_helper_matches_clock_observation() {
        let clock = PausedClock::new(epoch());
        let observed = now_wall_ms(clock.as_ref());
        let expected = epoch().unix_timestamp_nanos() / 1_000_000;
        assert_eq!(observed, expected as i64);
    }
}
