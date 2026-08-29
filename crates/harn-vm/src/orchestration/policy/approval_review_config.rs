//! The approval-reviewer policy, read once at the resolver seam.
//!
//! Data lives in `approval_review_policy.toml` beside this file; this module is
//! the typed shape it parses into and nothing else. The reviewer *session* --
//! prompt assembly, verdict schema, decision record -- is Harn
//! (`std/agent/approval_review`). Rust holds the seam, the way it holds the
//! provider catalog's seam while `providers.toml` holds the catalog.
//!
//! # Why the floor is checked here
//!
//! [`ApprovalReviewPolicy::is_floor`] runs *before* the reviewer is consulted.
//! A request naming a floor category never reaches a model at all, so no
//! session goal -- however plausibly worded, and whatever untrusted tool output
//! helped word it -- can talk the reviewer into granting one. A floor enforced
//! only inside the prompt would be a suggestion.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

const POLICY_TOML: &str = include_str!("approval_review_policy.toml");

/// The parsed bundled policy.
///
/// Parsed once. A malformed bundled policy is a build-time-shaped error that
/// can only be a Harn bug, so this panics rather than degrading to a permissive
/// default -- a reviewer running on a silently-empty policy would have no floor
/// and no denylist, which is the one failure mode worse than not running.
pub static APPROVAL_REVIEW_POLICY: LazyLock<ApprovalReviewPolicy> = LazyLock::new(|| {
    toml::from_str(POLICY_TOML).expect("bundled approval_review_policy.toml parses")
});

// No `Eq`: `BreakerConfig` carries a float share, and a policy is compared for
// display and tests rather than used as a key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalReviewPolicy {
    pub version: u32,
    pub reviewer: ReviewerConfig,
    pub breaker: BreakerConfig,
    pub floor: FloorConfig,
    pub denylist: DenylistConfig,
    pub trust: TrustConfig,
    pub verdict: VerdictConfig,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReviewerConfig {
    pub model: String,
    pub effort: String,
    pub timeout_ms: u64,
    /// What an unreachable or malformed reviewer means. Only `deny` is
    /// supported; the field exists so the fail-closed choice is visible in the
    /// policy a reader inspects, not buried in a match arm.
    pub on_error: OnReviewerError,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnReviewerError {
    #[default]
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BreakerConfig {
    pub max_consecutive_denials: u32,
    pub max_denials_per_turn: u32,
    pub cell_flag_denied_trial_share: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FloorConfig {
    pub never_grant: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DenylistConfig {
    pub categories: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustConfig {
    pub trusted_inputs: Vec<String>,
    pub untrusted_inputs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerdictConfig {
    pub risk_levels: Vec<String>,
    pub authorization_levels: Vec<String>,
    /// Risk -> the minimum authorization that may approve it. The literal
    /// `"never"` marks a risk no authorization can clear.
    pub thresholds: std::collections::BTreeMap<String, String>,
}

impl ApprovalReviewPolicy {
    pub fn bundled() -> &'static Self {
        &APPROVAL_REVIEW_POLICY
    }

    /// Whether `category` may never be granted, whatever the goal claims.
    pub fn is_floor(&self, category: &str) -> bool {
        self.floor.never_grant.iter().any(|c| c == category)
    }

    /// Whether `category` starts from a presumption of denial.
    pub fn is_denylisted(&self, category: &str) -> bool {
        self.denylist.categories.iter().any(|c| c == category)
    }

    /// Whether a verdict at `risk` may be approved at `authorization`.
    ///
    /// Unknown risk is treated as `critical`: a reviewer that returned a level
    /// this policy does not define has told us something we cannot interpret,
    /// and the safe reading of an uninterpretable verdict is the strictest one.
    pub fn approval_permitted(&self, risk: &str, authorization: &str) -> bool {
        let Some(required) = self.verdict.thresholds.get(risk) else {
            return false;
        };
        if required == "never" {
            return false;
        }
        let rank = |level: &str| {
            self.verdict
                .authorization_levels
                .iter()
                .position(|l| l == level)
        };
        match (rank(authorization), rank(required)) {
            (Some(have), Some(need)) => have >= need,
            // Same rule, same reason: an unrecognized authorization level is
            // not evidence of authority.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_policy_parses() {
        let policy = ApprovalReviewPolicy::bundled();
        assert_eq!(policy.version, 1);
        assert_eq!(policy.reviewer.on_error, OnReviewerError::Deny);
        assert!(policy.breaker.max_consecutive_denials > 0);
    }

    #[test]
    fn the_floor_is_not_empty() {
        // A floor that parsed to an empty list would let every category through
        // while still looking like a configured policy -- the absence-reads-as-
        // success shape, in the one place it would be worst.
        let policy = ApprovalReviewPolicy::bundled();
        assert!(!policy.floor.never_grant.is_empty());
        assert!(policy.is_floor("credential_exfiltration"));
        assert!(!policy.is_floor("read_a_source_file"));
    }

    #[test]
    fn critical_risk_can_never_be_approved() {
        let policy = ApprovalReviewPolicy::bundled();
        for authorization in &policy.verdict.authorization_levels {
            assert!(
                !policy.approval_permitted("critical", authorization),
                "critical must not be approvable at authorization {authorization}"
            );
        }
    }

    #[test]
    fn higher_authorization_clears_higher_risk() {
        let policy = ApprovalReviewPolicy::bundled();
        assert!(policy.approval_permitted("low", "unknown"));
        assert!(!policy.approval_permitted("high", "low"));
        assert!(policy.approval_permitted("high", "medium"));
        assert!(policy.approval_permitted("high", "high"));
    }

    #[test]
    fn an_uninterpretable_verdict_is_not_an_approval() {
        // Both directions: a risk level the policy does not define, and an
        // authorization level it does not define. A reviewer returning either
        // has said something we cannot read, and an unreadable verdict must not
        // resolve to yes.
        let policy = ApprovalReviewPolicy::bundled();
        assert!(!policy.approval_permitted("catastrophic", "high"));
        assert!(!policy.approval_permitted("low", "absolute"));
    }
}
