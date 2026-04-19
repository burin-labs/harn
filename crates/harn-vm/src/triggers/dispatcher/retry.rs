use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_ATTEMPTS: u32 = 7;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    #[default]
    Svix,
    Linear {
        delay_ms: u64,
    },
    Exponential {
        base_ms: u64,
        cap_ms: u64,
    },
}

impl RetryPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let attempt = attempt.max(1);
        match self {
            Self::Svix => {
                const SCHEDULE_MS: [u64; 8] = [
                    0,
                    5_000,
                    5 * 60_000,
                    30 * 60_000,
                    2 * 60 * 60_000,
                    5 * 60 * 60_000,
                    10 * 60 * 60_000,
                    10 * 60 * 60_000,
                ];
                let index = (attempt.saturating_sub(1) as usize).min(SCHEDULE_MS.len() - 1);
                Duration::from_millis(SCHEDULE_MS[index])
            }
            Self::Linear { delay_ms } => {
                if attempt == 1 {
                    Duration::ZERO
                } else {
                    Duration::from_millis(*delay_ms)
                }
            }
            Self::Exponential { base_ms, cap_ms } => {
                if attempt == 1 {
                    return Duration::ZERO;
                }
                let exponent = attempt.saturating_sub(2).min(63);
                let multiplier = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
                Duration::from_millis(base_ms.saturating_mul(multiplier).min(*cap_ms))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TriggerRetryConfig {
    pub max_attempts: u32,
    pub policy: RetryPolicy,
}

impl Default for TriggerRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            policy: RetryPolicy::default(),
        }
    }
}

impl TriggerRetryConfig {
    pub fn new(max_attempts: u32, policy: RetryPolicy) -> Self {
        Self {
            max_attempts,
            policy,
        }
    }

    pub fn max_attempts(&self) -> u32 {
        match self.max_attempts {
            0 => DEFAULT_MAX_ATTEMPTS,
            attempts => attempts,
        }
    }

    pub fn next_retry_delay(&self, completed_attempt: u32) -> Option<Duration> {
        let next_attempt = completed_attempt.saturating_add(1);
        if next_attempt > self.max_attempts() {
            None
        } else {
            Some(self.policy.delay_for_attempt(next_attempt))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RetryPolicy, TriggerRetryConfig};

    #[test]
    fn svix_schedule_matches_expected_vector() {
        let policy = RetryPolicy::Svix;
        let delays: Vec<u64> = (1..=8)
            .map(|attempt| policy.delay_for_attempt(attempt).as_millis() as u64)
            .collect();
        assert_eq!(
            delays,
            vec![
                0,
                5_000,
                5 * 60_000,
                30 * 60_000,
                2 * 60 * 60_000,
                5 * 60 * 60_000,
                10 * 60 * 60_000,
                10 * 60 * 60_000,
            ]
        );
    }

    #[test]
    fn linear_backoff_is_immediate_then_fixed() {
        let policy = RetryPolicy::Linear { delay_ms: 2_500 };
        let delays: Vec<u64> = (1..=4)
            .map(|attempt| policy.delay_for_attempt(attempt).as_millis() as u64)
            .collect();
        assert_eq!(delays, vec![0, 2_500, 2_500, 2_500]);
    }

    #[test]
    fn exponential_backoff_caps_at_the_configured_ceiling() {
        let policy = RetryPolicy::Exponential {
            base_ms: 1_000,
            cap_ms: 5_000,
        };
        let delays: Vec<u64> = (1..=6)
            .map(|attempt| policy.delay_for_attempt(attempt).as_millis() as u64)
            .collect();
        assert_eq!(delays, vec![0, 1_000, 2_000, 4_000, 5_000, 5_000]);
    }

    #[test]
    fn zero_max_attempts_defaults_to_seven_total_attempts() {
        let config = TriggerRetryConfig::new(0, RetryPolicy::Svix);
        assert_eq!(config.max_attempts(), 7);
        assert!(config.next_retry_delay(7).is_none());
    }
}
