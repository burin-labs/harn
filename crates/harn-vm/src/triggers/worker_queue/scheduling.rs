use serde::{Deserialize, Serialize};

pub(super) const NORMAL_PROMOTION_AGE_MS: i64 = 15 * 60 * 1000;

/// Maximum time deferrable work remains below higher-priority work.
pub const DEFERRABLE_PROMOTION_AGE_MS: i64 = 30 * 60 * 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerQueuePriority {
    High,
    #[default]
    Normal,
    Low,
}

impl WorkerQueuePriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    pub fn effective_rank(self, enqueued_at_ms: i64, now_ms: i64) -> u8 {
        match self {
            Self::High => 0,
            Self::Normal if now_ms.saturating_sub(enqueued_at_ms) >= NORMAL_PROMOTION_AGE_MS => 0,
            Self::Normal => 1,
            Self::Low if now_ms.saturating_sub(enqueued_at_ms) >= DEFERRABLE_PROMOTION_AGE_MS => 0,
            Self::Low => 2,
        }
    }

    pub fn promotion_deadline_at_ms(self, enqueued_at_ms: i64) -> Option<i64> {
        match self {
            Self::High => None,
            Self::Normal => Some(enqueued_at_ms.saturating_add(NORMAL_PROMOTION_AGE_MS)),
            Self::Low => Some(enqueued_at_ms.saturating_add(DEFERRABLE_PROMOTION_AGE_MS)),
        }
    }

    pub fn deadline_promoted(self, enqueued_at_ms: i64, now_ms: i64) -> bool {
        self.promotion_deadline_at_ms(enqueued_at_ms)
            .is_some_and(|deadline| now_ms >= deadline)
    }

    pub(in crate::triggers) fn selection_rank(self, enqueued_at_ms: i64, now_ms: i64) -> (u8, u8) {
        (
            u8::from(!self.deadline_promoted(enqueued_at_ms, now_ms)),
            self.effective_rank(enqueued_at_ms, now_ms),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerQueueSchedulingDecision {
    Priority,
    FairShare,
    StarvationDeadline,
}

/// Typed evidence for the scheduler decision that produced a claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerQueueSchedulingReceipt {
    pub selected_at_ms: i64,
    pub enqueued_at_ms: i64,
    pub waited_ms: u64,
    pub priority: WorkerQueuePriority,
    pub decision: WorkerQueueSchedulingDecision,
    pub promotion_deadline_at_ms: Option<i64>,
    pub fairness_key: String,
}
