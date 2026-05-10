//! Mockable wall-clock and monotonic clock for stdlib builtins.
//!
//! All time-sensitive builtins route through [`clock_mock`] so scripts can
//! pin time in tests via `mock_time(ms)` / `advance_time(ms)`. When a mock
//! is active, `sleep_ms` and `sleep` advance the mocked clock instead of
//! suspending the runtime — this lets tests exercise time-dependent logic
//! deterministically and converges with the trigger dispatcher / cron
//! scheduler, which read from the same mock stack.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::clock_mock;
use crate::testbench::tape::{self as tape, ClockSource, TapeRecordKind};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

static MONOTONIC_START: OnceLock<Instant> = OnceLock::new();

thread_local! {
    /// RAII guards held alive by `mock_time(...)` calls from Harn scripts.
    /// `unmock_time()` pops one. The per-test reset hook clears the lot.
    static MOCK_TIME_GUARDS: RefCell<Vec<clock_mock::ClockOverrideGuard>> =
        const { RefCell::new(Vec::new()) };
}

fn real_wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn real_monotonic_ms() -> i64 {
    let start = MONOTONIC_START.get_or_init(Instant::now);
    start.elapsed().as_millis() as i64
}

/// Current wall-clock time in milliseconds since UNIX_EPOCH.
/// Honors the active mock if one is installed.
pub fn now_wall_ms() -> i64 {
    let value = clock_mock::active_mock_clock()
        .map(|c| c.now_wall_ms())
        .unwrap_or_else(real_wall_ms);
    record_clock_read(ClockSource::Wall, value);
    value
}

/// Current wall-clock time in seconds (with fractional part).
pub fn now_wall_seconds() -> f64 {
    now_wall_ms() as f64 / 1000.0
}

/// Monotonic milliseconds. Honors the active mock; otherwise returns
/// elapsed millis since process start.
pub fn now_monotonic_ms() -> i64 {
    let value = clock_mock::active_mock_clock()
        .map(|c| c.now_monotonic_ms())
        .unwrap_or_else(real_monotonic_ms);
    record_clock_read(ClockSource::Monotonic, value);
    value
}

fn record_clock_read(source: ClockSource, value_ms: i64) {
    tape::with_active_recorder(|_recorder| Some(TapeRecordKind::ClockRead { source, value_ms }));
}

/// Whether a clock mock is currently active.
pub fn is_mocked() -> bool {
    clock_mock::is_mocked()
}

/// Advance the mocked clock by `ms` milliseconds. No-op if no mock is
/// installed (real time advances on its own).
pub fn advance(ms: i64) {
    if ms <= 0 {
        return;
    }
    clock_mock::advance(Duration::from_millis(ms as u64));
    tape::with_active_recorder(|_recorder| {
        Some(TapeRecordKind::ClockSleep {
            duration_ms: ms as u64,
        })
    });
}

fn push_mock(wall_ms: i64) {
    let clock = clock_mock::MockClock::at_wall_ms(wall_ms);
    let guard = clock_mock::install_override(clock);
    MOCK_TIME_GUARDS.with(|stack| stack.borrow_mut().push(guard));
}

fn pop_mock() -> bool {
    MOCK_TIME_GUARDS.with(|stack| stack.borrow_mut().pop().is_some())
}

/// Reset clock state for test isolation.
pub(crate) fn reset_clock_state() {
    MOCK_TIME_GUARDS.with(|stack| stack.borrow_mut().clear());
    clock_mock::clear_overrides();
}

/// RAII guard that installs a clock mock for the lifetime of the
/// guard and restores the previous state on drop. Use from Rust-side tests
/// that exercise builtins (`elapsed`, `now_ms`, `timestamp`, `sleep_ms`)
/// without needing to drive a `tokio::time::pause()` runtime.
///
/// Pairs with `tokio::time::pause()` for tests that span both worlds —
/// the tokio virtual clock pauses await-driven sleeps, while this guard
/// pauses the synchronous wall/monotonic clocks observed by stdlib code
/// and Harn scripts.
#[allow(dead_code)]
pub struct MockClockGuard {
    _override: clock_mock::ClockOverrideGuard,
}

#[allow(dead_code)]
impl MockClockGuard {
    /// Install a mock pinned to `wall_ms`. Monotonic counter starts at 0.
    pub fn install(wall_ms: i64) -> Self {
        let clock = clock_mock::MockClock::at_wall_ms(wall_ms);
        let guard = clock_mock::install_override(clock);
        Self { _override: guard }
    }

    /// Advance the mocked clock by `ms` milliseconds.
    pub fn advance(&self, ms: i64) {
        advance(ms);
    }

    /// Current mocked wall-clock millis.
    pub fn now_wall_ms(&self) -> i64 {
        now_wall_ms()
    }

    /// Current mocked monotonic millis.
    pub fn now_monotonic_ms(&self) -> i64 {
        now_monotonic_ms()
    }
}

pub(crate) fn register_clock_builtins(vm: &mut Vm) {
    // Replace the existing `timestamp` registration in process.rs so it
    // honors the mock. Process.rs registers it first; we override here.
    vm.register_builtin("timestamp", |_args, _out| {
        Ok(VmValue::Float(now_wall_seconds()))
    });

    // Replace `elapsed` so it honors the mock too.
    vm.register_builtin("elapsed", |_args, _out| {
        Ok(VmValue::Int(now_monotonic_ms()))
    });

    vm.register_builtin("monotonic_ms", |_args, _out| {
        Ok(VmValue::Int(now_monotonic_ms()))
    });

    vm.register_builtin("now_ms", |_args, _out| Ok(VmValue::Int(now_wall_ms())));

    vm.register_async_builtin("sleep_ms", |args| async move {
        let ms = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        if ms <= 0 {
            return Ok(VmValue::Nil);
        }
        if is_mocked() {
            advance(ms);
        } else {
            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("mock_time", |args, _out| {
        let Some(ms) = args.first().and_then(|a| a.as_int()) else {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "mock_time(ms): expected an integer millisecond timestamp",
            ))));
        };
        push_mock(ms);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("advance_time", |args, _out| {
        let ms = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        if !is_mocked() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "advance_time: no mock active. Call mock_time(ms) first.",
            ))));
        }
        advance(ms);
        Ok(VmValue::Int(now_wall_ms()))
    });

    vm.register_builtin("unmock_time", |_args, _out| {
        pop_mock();
        Ok(VmValue::Nil)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_overrides_wall_and_monotonic() {
        reset_clock_state();
        push_mock(1_000_000);
        assert_eq!(now_wall_ms(), 1_000_000);
        assert_eq!(now_monotonic_ms(), 0);
        advance(500);
        assert_eq!(now_wall_ms(), 1_000_500);
        assert_eq!(now_monotonic_ms(), 500);
        reset_clock_state();
        assert!(!is_mocked());
    }

    #[test]
    fn unmocked_real_time_progresses() {
        reset_clock_state();
        let a = now_wall_ms();
        std::thread::sleep(Duration::from_millis(2));
        let b = now_wall_ms();
        assert!(b >= a, "wall clock should not go backwards");
    }

    #[test]
    fn mock_clock_guard_restores_previous_state_on_drop() {
        reset_clock_state();
        assert!(!is_mocked());
        {
            let guard = MockClockGuard::install(2_000_000);
            assert!(is_mocked());
            assert_eq!(guard.now_wall_ms(), 2_000_000);
            guard.advance(100);
            assert_eq!(now_wall_ms(), 2_000_100);
            assert_eq!(now_monotonic_ms(), 100);
        }
        assert!(!is_mocked(), "guard should clear mock on drop");
    }

    #[test]
    fn mock_clock_guard_nests_and_restores_outer() {
        reset_clock_state();
        let outer = MockClockGuard::install(1_000);
        outer.advance(50);
        assert_eq!(now_wall_ms(), 1_050);
        {
            let inner = MockClockGuard::install(9_000);
            inner.advance(5);
            assert_eq!(now_wall_ms(), 9_005);
        }
        assert_eq!(now_wall_ms(), 1_050, "outer mock restored after inner drop");
        drop(outer);
        assert!(!is_mocked());
    }

    #[test]
    fn stdlib_mock_visible_to_trigger_module() {
        // Confirms the unification: stdlib mock_time installs an override
        // visible to the crate-wide clock_mock query.
        reset_clock_state();
        push_mock(123_000);
        let active = clock_mock::active_mock_clock().expect("active mock present");
        assert_eq!(active.now_wall_ms(), 123_000);
        advance(77);
        assert_eq!(active.now_wall_ms(), 123_077);
        reset_clock_state();
    }
}
