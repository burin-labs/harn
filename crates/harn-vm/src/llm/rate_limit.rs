//! Per-provider sliding-window rate limiter for outbound LLM requests.
//!
//! Proactively throttles requests to stay within configured RPM (requests per
//! minute) limits. When the window is full, `acquire_permit` yields to the
//! tokio scheduler via `sleep`, allowing other spawn_local tasks and parallel
//! pipelines to run.
//!
//! Configuration sources (later overrides earlier):
//! 1. `providers.toml` — `rpm` field on `ProviderDef`
//! 2. Environment variables — `HARN_RATE_LIMIT_<PROVIDER>=<rpm>`
//! 3. Runtime — `llm_rate_limit("provider", {rpm: N})` builtin

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const WINDOW_SECS: u64 = 60;

/// Per-provider sliding-window counter.
struct SlidingWindow {
    max_requests: u32,
    window: Duration,
    timestamps: VecDeque<Instant>,
}

impl SlidingWindow {
    fn new(rpm: u32) -> Self {
        Self {
            max_requests: rpm,
            window: Duration::from_secs(WINDOW_SECS),
            timestamps: VecDeque::with_capacity(rpm.min(1024) as usize),
        }
    }

    /// Drain expired entries and check capacity.
    /// Returns `Some(wait_duration)` if the window is full, `None` if OK.
    fn check(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let cutoff = now - self.window;
        while self.timestamps.front().is_some_and(|t| *t < cutoff) {
            self.timestamps.pop_front();
        }
        if (self.timestamps.len() as u32) < self.max_requests {
            None
        } else {
            // Wait until the oldest entry expires out of the window.
            let oldest = *self.timestamps.front().unwrap();
            Some((oldest + self.window).saturating_duration_since(now))
        }
    }

    /// Record a request timestamp.
    fn record(&mut self) {
        self.timestamps.push_back(Instant::now());
    }
}

thread_local! {
    static LIMITERS: RefCell<HashMap<String, SlidingWindow>> = RefCell::new(HashMap::new());
}

/// Load rate limits from provider config and environment variables.
/// Safe to call multiple times (replaces existing entries).
pub(crate) fn init_from_config() {
    let config = crate::llm_config::load_config();
    LIMITERS.with(|limiters| {
        let mut map = limiters.borrow_mut();
        for (name, pdef) in &config.providers {
            if let Some(rpm) = pdef.rpm {
                if rpm > 0 {
                    map.insert(name.clone(), SlidingWindow::new(rpm));
                }
            }
        }
    });
    // Environment overrides: HARN_RATE_LIMIT_TOGETHER=600, etc.
    for (key, val) in std::env::vars() {
        if let Some(provider) = key.strip_prefix("HARN_RATE_LIMIT_") {
            if let Ok(rpm) = val.parse::<u32>() {
                let provider = provider.to_lowercase();
                LIMITERS.with(|limiters| {
                    let mut map = limiters.borrow_mut();
                    if rpm > 0 {
                        map.insert(provider, SlidingWindow::new(rpm));
                    } else {
                        map.remove(&provider);
                    }
                });
            }
        }
    }
}

/// Set or update the rate limit for a provider at runtime.
pub(crate) fn set_rate_limit(provider: &str, rpm: u32) {
    LIMITERS.with(|limiters| {
        limiters
            .borrow_mut()
            .insert(provider.to_string(), SlidingWindow::new(rpm));
    });
}

/// Remove the rate limit for a provider.
pub(crate) fn clear_rate_limit(provider: &str) {
    LIMITERS.with(|limiters| {
        limiters.borrow_mut().remove(provider);
    });
}

/// Query the current RPM limit for a provider. Returns `None` if unlimited.
pub(crate) fn get_rate_limit(provider: &str) -> Option<u32> {
    LIMITERS.with(|limiters| limiters.borrow().get(provider).map(|sw| sw.max_requests))
}

/// Wait until the rate limit allows a request for this provider, then record it.
/// Returns immediately if no limit is configured or the window has capacity.
/// When throttled, yields to the tokio scheduler so other tasks can run.
pub(crate) async fn acquire_permit(provider: &str) {
    loop {
        let wait = LIMITERS.with(|limiters| {
            let mut map = limiters.borrow_mut();
            if let Some(sw) = map.get_mut(provider) {
                if let Some(duration) = sw.check() {
                    return Some(duration);
                }
                sw.record();
            }
            None
        });
        match wait {
            Some(duration) => {
                crate::events::log_debug(
                    "llm.rate_limit",
                    &format!(
                        "Rate limit for '{}': throttling for {}ms",
                        provider,
                        duration.as_millis()
                    ),
                );
                tokio::time::sleep(duration).await;
                // Re-check after sleep — another task may have consumed a slot.
            }
            None => return,
        }
    }
}

/// Reset all rate limiter state. Used between test runs.
pub(crate) fn reset_rate_limit_state() {
    LIMITERS.with(|limiters| limiters.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sliding_window_allows_within_limit() {
        let mut sw = SlidingWindow::new(3);
        assert!(sw.check().is_none());
        sw.record();
        assert!(sw.check().is_none());
        sw.record();
        assert!(sw.check().is_none());
        sw.record();
        // Window is now full.
        assert!(sw.check().is_some());
    }

    #[test]
    fn test_sliding_window_returns_wait_duration() {
        let mut sw = SlidingWindow::new(1);
        sw.record();
        let wait = sw.check();
        assert!(wait.is_some());
        let d = wait.unwrap();
        // Should be close to 60s (the full window).
        assert!(d.as_secs() <= 60);
        assert!(d.as_secs() >= 58);
    }

    #[test]
    fn test_set_and_get_rate_limit() {
        reset_rate_limit_state();
        assert_eq!(get_rate_limit("test_provider"), None);
        set_rate_limit("test_provider", 100);
        assert_eq!(get_rate_limit("test_provider"), Some(100));
        clear_rate_limit("test_provider");
        assert_eq!(get_rate_limit("test_provider"), None);
    }

    #[tokio::test]
    async fn test_acquire_permit_no_limit() {
        reset_rate_limit_state();
        // Should return immediately when no limit is configured.
        acquire_permit("unconfigured_provider").await;
    }

    #[tokio::test]
    async fn test_acquire_permit_within_limit() {
        reset_rate_limit_state();
        set_rate_limit("test_prov", 100);
        // Should return immediately when under the limit.
        acquire_permit("test_prov").await;
        acquire_permit("test_prov").await;
    }
}
