//! Pluggable rate-limit algorithms.
//!
//! Three implementations cover the common shapes a caller will reach for:
//!
//! * [`TokenBucket`] — classic refill-on-read bucket. Default. Good fit for
//!   "X requests per second with burst allowance Y".
//! * [`SlidingWindow`] — track timestamps over a rolling window. Fairer
//!   under steady load than token bucket (no edge bursts at window
//!   boundaries) but linear in number of in-window hits per check.
//! * [`LeakyBucket`] — drain at fixed rate; admit when level + 1 ≤ capacity.
//!   Useful for shaping outbound traffic ("smooth out spikes").
//!
//! Each implementation is per-key state — a single `RateAlgorithm` value
//! tracks one keyspace cell, e.g. one tenant. The store ([`super::store`])
//! owns the keyed map of these cells.

use std::collections::VecDeque;
use std::sync::Arc;

use harn_clock::Clock;

/// One trip through the algorithm — either admitted, or rejected with a
/// hint at how long the caller should back off before retrying. Callers
/// surface `retry_after_ms` through `Retry-After`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDecision {
    Allowed,
    Rejected { retry_after_ms: u64 },
}

/// Stateful rate-limit cell. Implementations hold one keyspace cell of
/// state (e.g. one tenant's bucket). The trait is object-safe so the
/// store can hold heterogeneous algorithm choices behind one map.
pub trait RateAlgorithm: Send {
    /// Attempt to admit one unit of work at `now_ms` (monotonic).
    fn try_admit(&mut self, now_ms: i64) -> CellDecision;
}

/// Identifies which algorithm to instantiate for a new keyspace cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Algorithm {
    /// Refill-on-read token bucket. Burst up to `capacity`; sustains at
    /// `rate_per_sec`.
    #[default]
    TokenBucket,
    /// Sliding-window log: admit up to `capacity` events per
    /// `window_ms` rolling window.
    SlidingWindow,
    /// Constant-rate leaky bucket: capacity is the queue depth, drains
    /// at `rate_per_sec`.
    LeakyBucket,
}

impl Algorithm {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "token_bucket" | "token-bucket" | "tokenbucket" => Some(Self::TokenBucket),
            "sliding_window" | "sliding-window" | "slidingwindow" => Some(Self::SlidingWindow),
            "leaky_bucket" | "leaky-bucket" | "leakybucket" => Some(Self::LeakyBucket),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TokenBucket => "token_bucket",
            Self::SlidingWindow => "sliding_window",
            Self::LeakyBucket => "leaky_bucket",
        }
    }

    /// Build a fresh per-key cell of the chosen algorithm. The
    /// `capacity` argument means different things by algorithm — see the
    /// individual impls — but always upper-bounds the burst tolerance.
    pub fn new_cell(
        self,
        rate_per_sec: f64,
        capacity: u32,
        clock: &Arc<dyn Clock>,
    ) -> Box<dyn RateAlgorithm> {
        let now_ms = clock.monotonic_ms();
        match self {
            Self::TokenBucket => Box::new(TokenBucket::new(rate_per_sec, capacity, now_ms)),
            Self::SlidingWindow => Box::new(SlidingWindow::new(
                capacity,
                window_ms_for_rate(rate_per_sec, capacity),
            )),
            Self::LeakyBucket => Box::new(LeakyBucket::new(rate_per_sec, capacity, now_ms)),
        }
    }
}

/// Derive the natural window size for a sliding-window cell from the
/// caller-supplied `rate_per_sec` and `capacity`: the window is sized so
/// that, on average, the configured rate sustains exactly `capacity`
/// hits per window.
fn window_ms_for_rate(rate_per_sec: f64, capacity: u32) -> u64 {
    if rate_per_sec <= 0.0 || capacity == 0 {
        return 1_000;
    }
    let window_secs = capacity as f64 / rate_per_sec;
    ((window_secs * 1_000.0).round() as u64).max(1)
}

// ── TokenBucket ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct TokenBucket {
    rate_per_sec: f64,
    capacity: f64,
    tokens: f64,
    last_refill_ms: i64,
}

impl TokenBucket {
    pub fn new(rate_per_sec: f64, capacity: u32, now_ms: i64) -> Self {
        Self {
            rate_per_sec: rate_per_sec.max(0.0),
            capacity: capacity as f64,
            tokens: capacity as f64,
            last_refill_ms: now_ms,
        }
    }

    fn refill(&mut self, now_ms: i64) {
        if now_ms <= self.last_refill_ms {
            return;
        }
        let delta_ms = (now_ms - self.last_refill_ms) as f64;
        let gained = (delta_ms / 1_000.0) * self.rate_per_sec;
        self.tokens = (self.tokens + gained).min(self.capacity);
        self.last_refill_ms = now_ms;
    }
}

impl RateAlgorithm for TokenBucket {
    fn try_admit(&mut self, now_ms: i64) -> CellDecision {
        self.refill(now_ms);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return CellDecision::Allowed;
        }
        // Need 1 - tokens more tokens; refill at rate_per_sec.
        let deficit = 1.0 - self.tokens;
        let retry_after_ms = if self.rate_per_sec > 0.0 {
            ((deficit / self.rate_per_sec) * 1_000.0).ceil() as u64
        } else {
            // No refill; effectively a hard cap. Tell the caller to wait
            // a second so they don't busy-spin.
            1_000
        };
        CellDecision::Rejected {
            retry_after_ms: retry_after_ms.max(1),
        }
    }
}

// ── SlidingWindow ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct SlidingWindow {
    capacity: usize,
    window_ms: u64,
    hits: VecDeque<i64>,
}

impl SlidingWindow {
    pub fn new(capacity: u32, window_ms: u64) -> Self {
        Self {
            capacity: capacity as usize,
            window_ms,
            hits: VecDeque::with_capacity(capacity as usize),
        }
    }

    fn trim(&mut self, now_ms: i64) {
        let cutoff = now_ms.saturating_sub(self.window_ms as i64);
        while let Some(&front) = self.hits.front() {
            if front <= cutoff {
                self.hits.pop_front();
            } else {
                break;
            }
        }
    }
}

impl RateAlgorithm for SlidingWindow {
    fn try_admit(&mut self, now_ms: i64) -> CellDecision {
        self.trim(now_ms);
        if self.capacity == 0 {
            return CellDecision::Rejected {
                retry_after_ms: self.window_ms.max(1),
            };
        }
        if self.hits.len() < self.capacity {
            self.hits.push_back(now_ms);
            return CellDecision::Allowed;
        }
        // Oldest hit expires at oldest + window; reject until then.
        let oldest = *self.hits.front().expect("hits non-empty when full");
        let earliest_open_ms = oldest + self.window_ms as i64;
        let retry_after_ms = ((earliest_open_ms - now_ms).max(1)) as u64;
        CellDecision::Rejected { retry_after_ms }
    }
}

// ── LeakyBucket ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct LeakyBucket {
    rate_per_sec: f64,
    capacity: f64,
    level: f64,
    last_drip_ms: i64,
}

impl LeakyBucket {
    pub fn new(rate_per_sec: f64, capacity: u32, now_ms: i64) -> Self {
        Self {
            rate_per_sec: rate_per_sec.max(0.0),
            capacity: capacity as f64,
            level: 0.0,
            last_drip_ms: now_ms,
        }
    }

    fn drain(&mut self, now_ms: i64) {
        if now_ms <= self.last_drip_ms {
            return;
        }
        let delta_ms = (now_ms - self.last_drip_ms) as f64;
        let drained = (delta_ms / 1_000.0) * self.rate_per_sec;
        self.level = (self.level - drained).max(0.0);
        self.last_drip_ms = now_ms;
    }
}

impl RateAlgorithm for LeakyBucket {
    fn try_admit(&mut self, now_ms: i64) -> CellDecision {
        self.drain(now_ms);
        if self.level + 1.0 <= self.capacity {
            self.level += 1.0;
            return CellDecision::Allowed;
        }
        // Need (level + 1) - capacity to drain at rate_per_sec.
        let overflow = (self.level + 1.0) - self.capacity;
        let retry_after_ms = if self.rate_per_sec > 0.0 {
            ((overflow / self.rate_per_sec) * 1_000.0).ceil() as u64
        } else {
            1_000
        };
        CellDecision::Rejected {
            retry_after_ms: retry_after_ms.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_admits_up_to_capacity_then_throttles() {
        let mut bucket = TokenBucket::new(1.0, 3, 0);
        assert_eq!(bucket.try_admit(0), CellDecision::Allowed);
        assert_eq!(bucket.try_admit(0), CellDecision::Allowed);
        assert_eq!(bucket.try_admit(0), CellDecision::Allowed);
        // Bucket is empty; 4th call rejects.
        match bucket.try_admit(0) {
            CellDecision::Rejected { retry_after_ms } => {
                // 1 second to refill 1 token at rate 1/sec.
                assert!((900..=1_100).contains(&retry_after_ms), "{retry_after_ms}");
            }
            other => panic!("expected reject, got {other:?}"),
        }
        // After 1 second, one token has refilled.
        assert_eq!(bucket.try_admit(1_000), CellDecision::Allowed);
    }

    #[test]
    fn sliding_window_rejects_when_full_until_oldest_drops_off() {
        let mut window = SlidingWindow::new(3, 1_000);
        assert_eq!(window.try_admit(0), CellDecision::Allowed);
        assert_eq!(window.try_admit(100), CellDecision::Allowed);
        assert_eq!(window.try_admit(200), CellDecision::Allowed);
        match window.try_admit(300) {
            CellDecision::Rejected { retry_after_ms } => assert_eq!(retry_after_ms, 700),
            other => panic!("expected reject, got {other:?}"),
        }
        // After the oldest expires (now_ms = 1_001), one slot frees up.
        assert_eq!(window.try_admit(1_001), CellDecision::Allowed);
    }

    #[test]
    fn leaky_bucket_smooths_burst_to_drain_rate() {
        // capacity 2 (queue depth), drain 1/sec
        let mut bucket = LeakyBucket::new(1.0, 2, 0);
        assert_eq!(bucket.try_admit(0), CellDecision::Allowed);
        assert_eq!(bucket.try_admit(0), CellDecision::Allowed);
        // Bucket is full; 3rd in same instant must reject.
        match bucket.try_admit(0) {
            CellDecision::Rejected { retry_after_ms } => {
                assert!((900..=1_100).contains(&retry_after_ms), "{retry_after_ms}");
            }
            other => panic!("expected reject, got {other:?}"),
        }
        // 1 second later one drop has drained → admit again.
        assert_eq!(bucket.try_admit(1_000), CellDecision::Allowed);
    }

    #[test]
    fn parse_algorithm_accepts_friendly_aliases() {
        assert_eq!(
            Algorithm::parse("token_bucket"),
            Some(Algorithm::TokenBucket)
        );
        assert_eq!(
            Algorithm::parse("TOKEN-BUCKET"),
            Some(Algorithm::TokenBucket)
        );
        assert_eq!(
            Algorithm::parse("sliding_window"),
            Some(Algorithm::SlidingWindow)
        );
        assert_eq!(
            Algorithm::parse("leaky_bucket"),
            Some(Algorithm::LeakyBucket)
        );
        assert_eq!(Algorithm::parse("nope"), None);
    }
}
