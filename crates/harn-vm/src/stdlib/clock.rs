//! Mockable wall-clock and monotonic clock.
//!
//! All time-sensitive builtins route through this module so scripts can
//! pin time in tests via `mock_time(ms)` / `advance_time(ms)`. When the
//! mock is active, `sleep_ms` advances the mocked clock instead of
//! suspending the runtime — this lets tests exercise time-dependent
//! logic deterministically.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

#[derive(Clone, Copy)]
struct ClockMock {
    /// Wall-clock millis since UNIX_EPOCH.
    wall_ms: i64,
    /// Monotonic millis (resets to 0 when the mock is installed).
    monotonic_ms: i64,
}

thread_local! {
    static CLOCK_MOCK: RefCell<Option<ClockMock>> = const { RefCell::new(None) };
}

static MONOTONIC_START: OnceLock<Instant> = OnceLock::new();

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
    CLOCK_MOCK
        .with(|m| m.borrow().map(|mock| mock.wall_ms))
        .unwrap_or_else(real_wall_ms)
}

/// Current wall-clock time in seconds (with fractional part).
pub fn now_wall_seconds() -> f64 {
    now_wall_ms() as f64 / 1000.0
}

/// Monotonic milliseconds. Honors the active mock; otherwise returns
/// elapsed millis since process start.
pub fn now_monotonic_ms() -> i64 {
    CLOCK_MOCK
        .with(|m| m.borrow().map(|mock| mock.monotonic_ms))
        .unwrap_or_else(real_monotonic_ms)
}

/// Whether a clock mock is currently active.
pub fn is_mocked() -> bool {
    CLOCK_MOCK.with(|m| m.borrow().is_some())
}

/// Advance the mocked clock by `ms` milliseconds. No-op if the mock is
/// not installed (real time advances on its own).
pub fn advance(ms: i64) {
    CLOCK_MOCK.with(|m| {
        if let Some(mock) = m.borrow_mut().as_mut() {
            mock.wall_ms = mock.wall_ms.saturating_add(ms);
            mock.monotonic_ms = mock.monotonic_ms.saturating_add(ms);
        }
    });
}

fn install_mock(wall_ms: i64) {
    CLOCK_MOCK.with(|m| {
        *m.borrow_mut() = Some(ClockMock {
            wall_ms,
            monotonic_ms: 0,
        });
    });
}

fn clear_mock() {
    CLOCK_MOCK.with(|m| *m.borrow_mut() = None);
}

/// Reset clock state for test isolation.
pub(crate) fn reset_clock_state() {
    clear_mock();
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
        install_mock(ms);
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
        clear_mock();
        Ok(VmValue::Nil)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_overrides_wall_and_monotonic() {
        clear_mock();
        install_mock(1_000_000);
        assert_eq!(now_wall_ms(), 1_000_000);
        assert_eq!(now_monotonic_ms(), 0);
        advance(500);
        assert_eq!(now_wall_ms(), 1_000_500);
        assert_eq!(now_monotonic_ms(), 500);
        clear_mock();
        assert!(!is_mocked());
    }

    #[test]
    fn unmocked_real_time_progresses() {
        clear_mock();
        let a = now_wall_ms();
        std::thread::sleep(Duration::from_millis(2));
        let b = now_wall_ms();
        assert!(b >= a, "wall clock should not go backwards");
    }
}
