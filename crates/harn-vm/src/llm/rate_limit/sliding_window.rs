use std::collections::VecDeque;
use std::time::Duration;

use super::WINDOW_SECS;

/// Weighted sliding-window counter.
///
/// Request buckets use one unit per request; token buckets use projected token
/// counts. A single request larger than the published per-minute quota is
/// charged as one full window so it can run, but the next request waits until
/// the window clears.
pub(super) struct SlidingWindow {
    max_units: u64,
    window_ms: u128,
    entries: VecDeque<(u128, u64)>,
}

impl SlidingWindow {
    pub(super) fn new(max_units: u64) -> Self {
        Self {
            max_units: max_units.max(1),
            window_ms: u128::from(WINDOW_SECS) * 1000,
            entries: VecDeque::with_capacity(max_units.min(1024) as usize),
        }
    }

    pub(super) fn prune(&mut self, now_ms: u128) {
        while self
            .entries
            .front()
            .is_some_and(|(t, _)| now_ms.saturating_sub(*t) >= self.window_ms)
        {
            self.entries.pop_front();
        }
    }

    pub(super) fn usage(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |acc, (_, units)| acc.saturating_add(*units))
    }

    fn charge(&self, units: u64) -> u64 {
        if units == 0 {
            0
        } else {
            units.min(self.max_units)
        }
    }

    /// Drain expired entries and check capacity.
    /// Returns `Some(wait_duration)` if the window is full, `None` if OK.
    pub(super) fn check(&mut self, now_ms: u128, units: u64) -> Option<Duration> {
        let charge = self.charge(units);
        if charge == 0 {
            return None;
        }
        self.prune(now_ms);
        let usage = self.usage();
        if usage.saturating_add(charge) <= self.max_units {
            return None;
        }
        let needed = usage.saturating_add(charge).saturating_sub(self.max_units);
        let mut freed = 0u64;
        for (entry_ms, units) in &self.entries {
            freed = freed.saturating_add(*units);
            if freed >= needed {
                let wait_ms = entry_ms
                    .saturating_add(self.window_ms)
                    .saturating_sub(now_ms);
                return Some(Duration::from_millis(
                    wait_ms.min(u128::from(u64::MAX)) as u64
                ));
            }
        }
        Some(Duration::from_millis(
            self.window_ms.min(u128::from(u64::MAX)) as u64,
        ))
    }

    /// Record a request or token charge timestamp.
    pub(super) fn record(&mut self, now_ms: u128, units: u64) {
        let charge = self.charge(units);
        if charge > 0 {
            self.entries.push_back((now_ms, charge));
        }
    }
}
