//! Per-pool circuit breaker.
//!
//! Tracks consecutive failures against a Postgres pool and short-circuits
//! queries once a threshold is hit. Half-open probes let traffic resume
//! gradually after the configured `reset_after_ms` cooldown.
//!
//! State is purely in-process. Failures are detected at the call-site
//! (every `query_rows` / `execute_stmt` records the outcome). When the
//! circuit is open, callers fast-fail with `pg: circuit open` instead of
//! waiting for the connection-acquire timeout. A single probe is allowed
//! in the half-open state; if it succeeds the circuit closes, if it
//! fails the cooldown restarts.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-pool failure tracking state. Shared via Arc.
pub(in crate::stdlib) struct CircuitBreakerState {
    pub(in crate::stdlib) failure_threshold: u32,
    pub(in crate::stdlib) reset_after_ms: i64,
    pub(in crate::stdlib) failures: AtomicU32,
    pub(in crate::stdlib) opened_at_ms: AtomicI64,
    pub(in crate::stdlib) half_open_in_flight: Mutex<bool>,
}

impl CircuitBreakerState {
    pub(in crate::stdlib) fn new(failure_threshold: u32, reset_after_ms: i64) -> Self {
        Self {
            failure_threshold,
            reset_after_ms,
            failures: AtomicU32::new(0),
            opened_at_ms: AtomicI64::new(0),
            half_open_in_flight: Mutex::new(false),
        }
    }

    pub(in crate::stdlib) fn disabled() -> Self {
        Self::new(0, 0)
    }

    pub(in crate::stdlib) fn is_enabled(&self) -> bool {
        self.failure_threshold > 0
    }

    /// Decide whether a new operation may proceed.
    /// - `Allow::Closed` — pass through normally.
    /// - `Allow::Probe` — circuit was open + cooldown elapsed; reserve a
    ///   single in-flight probe.
    /// - `Allow::Open` — fast-fail.
    pub(in crate::stdlib) fn admit(&self) -> Allow {
        if !self.is_enabled() {
            return Allow::Closed;
        }
        let opened_at = self.opened_at_ms.load(Ordering::Acquire);
        if opened_at == 0 {
            return Allow::Closed;
        }
        let now = now_ms();
        if now.saturating_sub(opened_at) < self.reset_after_ms {
            return Allow::Open;
        }
        // Cooldown elapsed: reserve a half-open probe. Only one probe is
        // allowed at a time — concurrent callers see Open until it resolves.
        let mut guard = match self.half_open_in_flight.try_lock() {
            Ok(g) => g,
            Err(_) => return Allow::Open,
        };
        if *guard {
            return Allow::Open;
        }
        *guard = true;
        Allow::Probe
    }

    pub(in crate::stdlib) fn record_success(&self, was_probe: bool) {
        if !self.is_enabled() {
            return;
        }
        self.failures.store(0, Ordering::Release);
        self.opened_at_ms.store(0, Ordering::Release);
        if was_probe {
            if let Ok(mut g) = self.half_open_in_flight.lock() {
                *g = false;
            }
        }
    }

    pub(in crate::stdlib) fn record_failure(&self, was_probe: bool) {
        if !self.is_enabled() {
            return;
        }
        if was_probe {
            // Half-open probe failed: restart the cooldown and clear the
            // in-flight bit so the next admit() can re-probe later.
            self.opened_at_ms.store(now_ms(), Ordering::Release);
            if let Ok(mut g) = self.half_open_in_flight.lock() {
                *g = false;
            }
            return;
        }
        let prev = self.failures.fetch_add(1, Ordering::AcqRel);
        if prev + 1 >= self.failure_threshold {
            self.opened_at_ms.store(now_ms(), Ordering::Release);
        }
    }

    pub(in crate::stdlib) fn snapshot(&self) -> CircuitSnapshot {
        if !self.is_enabled() {
            return CircuitSnapshot {
                state: "disabled",
                failures: 0,
                opened_at_ms: None,
            };
        }
        let opened_at = self.opened_at_ms.load(Ordering::Acquire);
        let state = if opened_at == 0 {
            "closed"
        } else if now_ms().saturating_sub(opened_at) >= self.reset_after_ms {
            "half_open"
        } else {
            "open"
        };
        CircuitSnapshot {
            state,
            failures: self.failures.load(Ordering::Acquire),
            opened_at_ms: if opened_at == 0 {
                None
            } else {
                Some(opened_at)
            },
        }
    }
}

pub(in crate::stdlib) enum Allow {
    Closed,
    Probe,
    Open,
}

pub(in crate::stdlib) struct CircuitSnapshot {
    pub(in crate::stdlib) state: &'static str,
    pub(in crate::stdlib) failures: u32,
    pub(in crate::stdlib) opened_at_ms: Option<i64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_breaker_always_allows() {
        let cb = CircuitBreakerState::disabled();
        assert!(matches!(cb.admit(), Allow::Closed));
        cb.record_failure(false);
        cb.record_failure(false);
        cb.record_failure(false);
        assert!(matches!(cb.admit(), Allow::Closed));
    }

    #[test]
    fn opens_after_threshold_then_probes() {
        let cb = CircuitBreakerState::new(3, 50);
        cb.record_failure(false);
        cb.record_failure(false);
        assert!(matches!(cb.admit(), Allow::Closed));
        cb.record_failure(false);
        // After the third failure, circuit is open.
        assert!(matches!(cb.admit(), Allow::Open));
        std::thread::sleep(std::time::Duration::from_millis(60));
        // After cooldown, one probe is admitted.
        let first = cb.admit();
        assert!(matches!(first, Allow::Probe));
        // A second concurrent caller sees Open while probe is in flight.
        assert!(matches!(cb.admit(), Allow::Open));
        cb.record_success(true);
        // Success closes the circuit.
        assert!(matches!(cb.admit(), Allow::Closed));
    }

    #[test]
    fn failed_probe_restarts_cooldown() {
        let cb = CircuitBreakerState::new(1, 30);
        cb.record_failure(false);
        assert!(matches!(cb.admit(), Allow::Open));
        std::thread::sleep(std::time::Duration::from_millis(40));
        let probe = cb.admit();
        assert!(matches!(probe, Allow::Probe));
        cb.record_failure(true);
        // Cooldown restarts; immediate retry is Open.
        assert!(matches!(cb.admit(), Allow::Open));
    }

    #[test]
    fn snapshot_reports_state_transitions() {
        let cb = CircuitBreakerState::new(2, 20);
        let s = cb.snapshot();
        assert_eq!(s.state, "closed");
        cb.record_failure(false);
        cb.record_failure(false);
        let s = cb.snapshot();
        assert_eq!(s.state, "open");
        assert_eq!(s.failures, 2);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let s = cb.snapshot();
        assert_eq!(s.state, "half_open");
    }
}
