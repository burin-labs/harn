//! Audit trail for every permission decision. The store appends an
//! [`AuditEntry`] for each `record_decision` call regardless of the
//! verdict, so a denial, an auto-allow from a remember-rule, and a
//! human approval all look the same to a downstream observer.
//!
//! The eventual A.5 session-store will subscribe to the same stream
//! and forward entries onto a tenant's `PermissionDecision` event
//! feed; until then the in-memory ring keeps the most recent N events
//! queryable through `store.history(filter)`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::policy::PolicyVersion;
use super::request::{DecisionScope, PermissionRequest, Risk};
use super::rules::RuleId;

/// Categorical outcome — derived from the decision the store
/// returned. Kept separate from `PermissionDecision` so audit
/// consumers can group by outcome without parsing the decision shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Granted,
    Denied,
    Escalated,
}

/// One audit record. Carries enough context to reconstruct the
/// decision: the original request, the verdict, the policy version it
/// was evaluated against, and (when applicable) the rule that
/// satisfied it. Decided-at vs requested-at differ when the request
/// suspended for human review.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub request: PermissionRequest,
    pub outcome: AuditOutcome,
    pub scope: Option<DecisionScope>,
    pub policy_version: PolicyVersion,
    pub risk: Risk,
    pub rule_id: Option<RuleId>,
    pub reason: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
    pub decided_at: OffsetDateTime,
    /// Approver identity — `Some` when a human or escalator made the
    /// call, `None` when a remember-rule or policy auto-decided.
    pub decided_by: Option<String>,
}

/// Filter for querying audit history. Every field is `Option` and
/// applied conjunctively; `None` means "any". `limit` caps the result
/// set; the store always returns most-recent-first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditFilter {
    pub tenant_id: Option<String>,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub actor: Option<String>,
    pub outcome: Option<AuditOutcome>,
    pub since: Option<OffsetDateTime>,
    pub limit: Option<usize>,
}

impl AuditFilter {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        if let Some(tenant) = self.tenant_id.as_deref() {
            if entry.request.tenant_id.as_deref() != Some(tenant) {
                return false;
            }
        }
        if let Some(session) = self.session_id.as_deref() {
            if entry.request.session_id != session {
                return false;
            }
        }
        if let Some(workspace) = self.workspace_id.as_deref() {
            if entry.request.workspace_id.as_deref() != Some(workspace) {
                return false;
            }
        }
        if let Some(actor) = self.actor.as_deref() {
            if entry.request.actor != actor {
                return false;
            }
        }
        if let Some(outcome) = self.outcome {
            if entry.outcome != outcome {
                return false;
            }
        }
        if let Some(since) = self.since {
            if entry.decided_at < since {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::request::ActionClass;

    fn entry(session: &str, outcome: AuditOutcome) -> AuditEntry {
        let mut req =
            PermissionRequest::new("p1", session, "alice", ActionClass::Read, "fs.read", "x");
        req.tenant_id = Some("tenant-a".to_string());
        AuditEntry {
            request: req,
            outcome,
            scope: Some(DecisionScope::Session),
            policy_version: PolicyVersion::empty(),
            risk: Risk::Low,
            rule_id: None,
            reason: None,
            expires_at: None,
            decided_at: OffsetDateTime::now_utc(),
            decided_by: None,
        }
    }

    #[test]
    fn empty_filter_matches_any_entry() {
        let filter = AuditFilter::default();
        assert!(filter.matches(&entry("s1", AuditOutcome::Granted)));
    }

    #[test]
    fn filter_dimensions_apply_conjunctively() {
        let filter = AuditFilter {
            session_id: Some("s1".into()),
            outcome: Some(AuditOutcome::Granted),
            ..Default::default()
        };
        assert!(filter.matches(&entry("s1", AuditOutcome::Granted)));
        assert!(!filter.matches(&entry("s1", AuditOutcome::Denied)));
        assert!(!filter.matches(&entry("s2", AuditOutcome::Granted)));
    }

    #[test]
    fn since_filter_keeps_entries_at_or_after_threshold() {
        let mut older = entry("s1", AuditOutcome::Granted);
        older.decided_at = OffsetDateTime::now_utc() - time::Duration::hours(2);
        let mut newer = entry("s1", AuditOutcome::Granted);
        newer.decided_at = OffsetDateTime::now_utc();
        let filter = AuditFilter {
            since: Some(OffsetDateTime::now_utc() - time::Duration::hours(1)),
            ..Default::default()
        };
        assert!(!filter.matches(&older));
        assert!(filter.matches(&newer));
    }
}
