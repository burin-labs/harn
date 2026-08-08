//! [`PermissionStore`] — the trait every surface (REST, ACP, eventual
//! `harness.permissions.*` host calls) talks to — plus the in-memory
//! implementation that ships today.
//!
//! The trait deliberately keeps both halves of the request lifecycle
//! ([`evaluate`](PermissionStore::evaluate),
//! [`record_decision`](PermissionStore::record_decision)) inside the
//! same store so a single mutex protects the rule set and the audit
//! ring. A future Postgres-backed implementation can swap in via the
//! same shape; A.5 will subscribe to the audit feed to forward
//! [`AuditEntry`] events onto a tenant's session stream.
//!
//! The in-memory store is `Send + Sync` and cheap to clone (an
//! `Arc<Mutex<...>>` inside); every adapter mounts the same instance.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::OffsetDateTime;

use super::audit::{AuditEntry, AuditFilter, AuditOutcome};
use super::policy::{PermissionPolicy, PolicyVersion};
use super::request::{ActionClass, DecisionScope, PermissionDecision, PermissionRequest, Risk};
use super::rules::{RememberRule, RuleId};

/// The contract every consumer surface depends on. The trait is
/// `async_trait` because the durable backend will be async (Postgres
/// pool, A.5 session stream); the in-memory implementation satisfies
/// the trait without ever yielding.
#[async_trait]
pub trait PermissionStore: Send + Sync {
    /// Evaluate `request` against the live policy and rule set. The
    /// store never blocks waiting for a human — when the decision
    /// requires escalation it returns
    /// [`PermissionDecision::Suspend`] and the caller is expected to
    /// hand it off to the ACP `session/request_permission` channel.
    async fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision;

    /// Record a final decision (typically the one returned by
    /// `evaluate`, or the human verdict that resolved a previously-
    /// suspended request). The store appends an audit entry and, when
    /// `remember` is `Some`, materializes a [`RememberRule`] so
    /// subsequent matching requests resolve without escalation.
    async fn record_decision(
        &self,
        request: &PermissionRequest,
        decision: &PermissionDecision,
        decided_by: Option<String>,
        remember: Option<RememberSpec>,
    );

    /// Install a [`PermissionPolicy`]. Replaces the entire policy
    /// (the store doesn't merge — callers compose externally so the
    /// composition rules stay testable as pure data). Returns the
    /// new version.
    async fn install_policy(&self, policy: PermissionPolicy) -> PolicyVersion;

    /// Current installed policy.
    async fn policy(&self) -> PermissionPolicy;

    /// All non-revoked, non-expired rules, narrowest scope first.
    async fn rules(&self) -> Vec<RememberRule>;

    /// Add a rule explicitly (without going through a decision).
    /// Used by the REST API's `POST /v1/permissions/rules` and by the
    /// initial-load path that hydrates from a backing store.
    async fn add_rule(&self, rule: RememberRule);

    /// Soft-revoke a rule. The audit history preserves the original
    /// rule; subsequent evaluations skip it.
    async fn revoke_rule(&self, id: &RuleId) -> bool;

    /// Query the audit ring.
    async fn history(&self, filter: &AuditFilter) -> Vec<AuditEntry>;
}

/// Request to "remember" a decision as a rule. The scope + action
/// pattern + target pattern are filled by the caller; the store
/// converts the spec into a [`RememberRule`] keyed off the request's
/// tenant / session / workspace / actor.
#[derive(Clone, Debug)]
pub struct RememberSpec {
    pub scope: DecisionScope,
    pub action_pattern: Option<String>,
    pub target_pattern: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
}

/// Configuration knobs on the in-memory store. The audit ring caps
/// memory; the autodeny floor lets a deployment tighten the default
/// fall-through for high-risk requests when no policy / rule applies
/// (e.g. block all `Risk::Critical` even before a human sees it).
#[derive(Clone, Debug)]
pub struct InMemoryConfig {
    pub audit_capacity: usize,
    /// Fallback verdict for `Risk` tiers at or above this threshold
    /// when nothing in the policy or rules applies. `None` defers to
    /// escalation always.
    pub auto_deny_at_or_above: Option<Risk>,
}

impl Default for InMemoryConfig {
    fn default() -> Self {
        Self {
            audit_capacity: 1024,
            auto_deny_at_or_above: None,
        }
    }
}

/// Default in-memory implementation. Holds an active policy, the rule
/// list, and a ring buffer of audit entries. Cheap to clone — the
/// state lives behind an `Arc<Mutex<...>>` so every adapter shares
/// the same view.
#[derive(Clone)]
pub struct InMemoryPermissionStore {
    inner: Arc<Mutex<StoreInner>>,
    config: InMemoryConfig,
}

struct StoreInner {
    policy: PermissionPolicy,
    policy_version: PolicyVersion,
    rules: Vec<RememberRule>,
    audit: VecDeque<AuditEntry>,
}

impl Default for InMemoryPermissionStore {
    fn default() -> Self {
        Self::new(InMemoryConfig::default())
    }
}

impl InMemoryPermissionStore {
    pub fn new(config: InMemoryConfig) -> Self {
        let policy = PermissionPolicy::empty();
        let version = policy.version();
        Self {
            inner: Arc::new(Mutex::new(StoreInner {
                policy,
                policy_version: version,
                rules: Vec::new(),
                audit: VecDeque::with_capacity(config.audit_capacity),
            })),
            config,
        }
    }

    /// Construct with an initial policy already installed. The
    /// policy version captured here is what subsequent audit entries
    /// pin against until `install_policy` is called again.
    pub fn with_policy(policy: PermissionPolicy, config: InMemoryConfig) -> Self {
        let store = Self::new(config);
        let version = policy.version();
        {
            let mut inner = store.inner.lock().expect("permission store poisoned");
            inner.policy = policy;
            inner.policy_version = version;
        }
        store
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn push_audit(inner: &mut StoreInner, capacity: usize, entry: AuditEntry) {
        if inner.audit.len() == capacity {
            inner.audit.pop_front();
        }
        inner.audit.push_back(entry);
    }
}

#[async_trait]
impl PermissionStore for InMemoryPermissionStore {
    async fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        let now = Self::now();
        let inner = self.inner.lock().expect("permission store poisoned");

        // Walk rules narrowest-scope-first. The store maintains rules
        // in insertion order; sort by scope at evaluation time so a
        // newly-added Workspace rule still loses to a pre-existing
        // Session rule.
        let mut rules_by_scope: Vec<&RememberRule> = inner.rules.iter().collect();
        rules_by_scope.sort_by_key(|rule| rule.scope);
        for rule in rules_by_scope {
            if rule.matches(request, now) {
                return if rule.allow {
                    PermissionDecision::Granted {
                        scope: rule.scope,
                        policy_version: inner.policy_version.clone(),
                        reason: rule.reason.clone(),
                        expires_at: rule.expires_at,
                        rule_id: Some(rule.id.clone()),
                    }
                } else {
                    PermissionDecision::Denied {
                        scope: rule.scope,
                        policy_version: inner.policy_version.clone(),
                        reason: rule.reason.clone(),
                        rule_id: Some(rule.id.clone()),
                    }
                };
            }
        }

        // No rule applied — consult the policy.
        let policy_allow = match request.class {
            ActionClass::Llm => {
                let providers = &inner.policy.llm.providers;
                !providers.is_empty() && providers.contains(&request.target)
            }
            ActionClass::Custom => false,
            class => inner.policy.matcher_for(class).matches(&request.target),
        };

        if policy_allow {
            return PermissionDecision::Granted {
                scope: DecisionScope::Workspace,
                policy_version: inner.policy_version.clone(),
                reason: Some(format!(
                    "allowed by policy ({class})",
                    class = request.class.name()
                )),
                expires_at: None,
                rule_id: None,
            };
        }

        // Auto-deny for risk tiers at or above the configured floor.
        let effective_risk = request.effective_risk();
        if let Some(floor) = self.config.auto_deny_at_or_above {
            if effective_risk >= floor {
                return PermissionDecision::Denied {
                    scope: DecisionScope::Session,
                    policy_version: inner.policy_version.clone(),
                    reason: Some(format!(
                        "auto-denied: risk {effective_risk:?} >= floor {floor:?}",
                    )),
                    rule_id: None,
                };
            }
        }

        // Fall through to escalation.
        let escalate_to = if inner.policy.escalate_to.is_empty() {
            vec!["user".to_string()]
        } else {
            inner.policy.escalate_to.clone()
        };
        PermissionDecision::Suspend {
            policy_version: inner.policy_version.clone(),
            escalate_to,
            reason: request.reason.clone(),
        }
    }

    async fn record_decision(
        &self,
        request: &PermissionRequest,
        decision: &PermissionDecision,
        decided_by: Option<String>,
        remember: Option<RememberSpec>,
    ) {
        let now = Self::now();
        let (outcome, scope, expires_at, rule_id_from_decision) = match decision {
            PermissionDecision::Granted {
                scope,
                expires_at,
                rule_id,
                ..
            } => (
                AuditOutcome::Granted,
                Some(*scope),
                *expires_at,
                rule_id.clone(),
            ),
            PermissionDecision::Denied { scope, rule_id, .. } => {
                (AuditOutcome::Denied, Some(*scope), None, rule_id.clone())
            }
            PermissionDecision::Suspend { .. } => (AuditOutcome::Escalated, None, None, None),
        };

        let mut inner = self.inner.lock().expect("permission store poisoned");
        let mut materialized_rule_id = rule_id_from_decision;
        if let Some(spec) = remember {
            if matches!(
                decision,
                PermissionDecision::Granted { .. } | PermissionDecision::Denied { .. }
            ) {
                let allow = decision.is_granted();
                if let Ok(mut rule) = RememberRule::new(
                    spec.scope,
                    scope_value_for(spec.scope, request),
                    request.class,
                    spec.action_pattern
                        .unwrap_or_else(|| request.action.clone()),
                    spec.target_pattern
                        .unwrap_or_else(|| request.target.clone()),
                    allow,
                    request.actor.clone(),
                ) {
                    rule.tenant_id = request.tenant_id.clone();
                    rule.expires_at = spec.expires_at;
                    rule.reason = decision.reason().map(str::to_string);
                    materialized_rule_id = Some(rule.id.clone());
                    inner.rules.push(rule);
                }
            }
        }

        let entry = AuditEntry {
            request: request.clone(),
            outcome,
            scope,
            policy_version: decision.policy_version().clone(),
            risk: request.effective_risk(),
            rule_id: materialized_rule_id,
            reason: decision.reason().map(str::to_string),
            expires_at,
            decided_at: now,
            decided_by,
        };
        Self::push_audit(&mut inner, self.config.audit_capacity, entry);
    }

    async fn install_policy(&self, policy: PermissionPolicy) -> PolicyVersion {
        let version = policy.version();
        let mut inner = self.inner.lock().expect("permission store poisoned");
        inner.policy = policy;
        inner.policy_version = version.clone();
        version
    }

    async fn policy(&self) -> PermissionPolicy {
        self.inner
            .lock()
            .expect("permission store poisoned")
            .policy
            .clone()
    }

    async fn rules(&self) -> Vec<RememberRule> {
        let now = Self::now();
        let inner = self.inner.lock().expect("permission store poisoned");
        inner
            .rules
            .iter()
            .filter(|rule| rule.revoked_at.is_none())
            .filter(|rule| rule.expires_at.map(|expires| expires > now).unwrap_or(true))
            .cloned()
            .collect()
    }

    async fn add_rule(&self, mut rule: RememberRule) {
        let _ = rule.ensure_compiled();
        let mut inner = self.inner.lock().expect("permission store poisoned");
        inner.rules.push(rule);
    }

    async fn revoke_rule(&self, id: &RuleId) -> bool {
        let mut inner = self.inner.lock().expect("permission store poisoned");
        for rule in inner.rules.iter_mut() {
            if rule.id == *id && rule.revoked_at.is_none() {
                rule.revoked_at = Some(Self::now());
                return true;
            }
        }
        false
    }

    async fn history(&self, filter: &AuditFilter) -> Vec<AuditEntry> {
        let inner = self.inner.lock().expect("permission store poisoned");
        let mut out: Vec<AuditEntry> = inner
            .audit
            .iter()
            .rev()
            .filter(|entry| filter.matches(entry))
            .cloned()
            .collect();
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        out
    }
}

fn scope_value_for(scope: DecisionScope, request: &PermissionRequest) -> Option<String> {
    match scope {
        DecisionScope::Session => Some(request.session_id.clone()),
        DecisionScope::Workspace => request.workspace_id.clone(),
        DecisionScope::User => Some(request.actor.clone()),
        DecisionScope::Always => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::policy::PermissionPolicy;
    use crate::permissions::request::{ActionClass, Risk};

    fn build_request(class: ActionClass, action: &str, target: &str) -> PermissionRequest {
        let mut req = PermissionRequest::new("p1", "s1", "alice", class, action, target);
        req.workspace_id = Some("ws".to_string());
        req
    }

    #[tokio::test]
    async fn no_policy_escalates_by_default() {
        let store = InMemoryPermissionStore::default();
        let request = build_request(ActionClass::Read, "fs.read", "src/lib.rs");
        let decision = store.evaluate(&request).await;
        assert!(matches!(decision, PermissionDecision::Suspend { .. }));
    }

    #[tokio::test]
    async fn policy_glob_auto_grants_matching_request() {
        let mut policy = PermissionPolicy::empty();
        policy.read = vec!["src/**".to_string()];
        let store = InMemoryPermissionStore::with_policy(policy, InMemoryConfig::default());
        let request = build_request(ActionClass::Read, "fs.read", "src/lib.rs");
        let decision = store.evaluate(&request).await;
        assert!(matches!(decision, PermissionDecision::Granted { .. }));
    }

    #[tokio::test]
    async fn session_rule_overrides_workspace_rule() {
        let store = InMemoryPermissionStore::default();
        let workspace_rule = RememberRule::new(
            DecisionScope::Workspace,
            Some("ws".to_string()),
            ActionClass::Write,
            "fs.write",
            "src/**",
            true,
            "alice",
        )
        .unwrap();
        let session_rule = RememberRule::new(
            DecisionScope::Session,
            Some("s1".to_string()),
            ActionClass::Write,
            "fs.write",
            "src/**",
            false,
            "alice",
        )
        .unwrap();
        store.add_rule(workspace_rule).await;
        store.add_rule(session_rule).await;
        let request = build_request(ActionClass::Write, "fs.write", "src/lib.rs");
        let decision = store.evaluate(&request).await;
        assert!(matches!(
            decision,
            PermissionDecision::Denied {
                scope: DecisionScope::Session,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn remember_spec_materializes_rule() {
        let store = InMemoryPermissionStore::default();
        let request = build_request(ActionClass::Read, "fs.read", "src/lib.rs");
        let decision = PermissionDecision::Granted {
            scope: DecisionScope::Workspace,
            policy_version: PolicyVersion::empty(),
            reason: Some("approved by alice".to_string()),
            expires_at: None,
            rule_id: None,
        };
        store
            .record_decision(
                &request,
                &decision,
                Some("alice".into()),
                Some(RememberSpec {
                    scope: DecisionScope::Workspace,
                    action_pattern: Some("fs.*".into()),
                    target_pattern: Some("src/**".into()),
                    expires_at: None,
                }),
            )
            .await;
        let rules = store.rules().await;
        assert_eq!(rules.len(), 1);
        let again = store.evaluate(&request).await;
        assert!(matches!(
            again,
            PermissionDecision::Granted {
                rule_id: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn revoke_rule_removes_from_active_set() {
        let store = InMemoryPermissionStore::default();
        let rule = RememberRule::new(
            DecisionScope::Always,
            None,
            ActionClass::Read,
            "*",
            "**",
            true,
            "system",
        )
        .unwrap();
        let id = rule.id.clone();
        store.add_rule(rule).await;
        assert_eq!(store.rules().await.len(), 1);
        assert!(store.revoke_rule(&id).await);
        assert_eq!(store.rules().await.len(), 0);
        // Second revoke is a no-op.
        assert!(!store.revoke_rule(&id).await);
    }

    #[tokio::test]
    async fn expired_grant_is_skipped_from_rules() {
        let store = InMemoryPermissionStore::default();
        let mut rule = RememberRule::new(
            DecisionScope::Always,
            None,
            ActionClass::Read,
            "*",
            "**",
            true,
            "system",
        )
        .unwrap();
        rule.expires_at = Some(OffsetDateTime::now_utc() - time::Duration::seconds(1));
        store.add_rule(rule).await;
        assert_eq!(store.rules().await.len(), 0);
    }

    #[tokio::test]
    async fn auto_deny_floor_blocks_critical_requests() {
        let config = InMemoryConfig {
            auto_deny_at_or_above: Some(Risk::Critical),
            ..Default::default()
        };
        let store = InMemoryPermissionStore::new(config);
        let mut request = build_request(ActionClass::Exec, "shell.exec", "rm -rf /");
        request.risk = Some(Risk::Critical);
        let decision = store.evaluate(&request).await;
        assert!(matches!(decision, PermissionDecision::Denied { .. }));
    }

    #[tokio::test]
    async fn audit_ring_caps_history() {
        let config = InMemoryConfig {
            audit_capacity: 2,
            ..Default::default()
        };
        let store = InMemoryPermissionStore::new(config);
        for i in 0..5 {
            let mut req = build_request(ActionClass::Read, "fs.read", &format!("file-{i}"));
            req.id = format!("p{i}");
            let decision = PermissionDecision::Granted {
                scope: DecisionScope::Session,
                policy_version: PolicyVersion::empty(),
                reason: None,
                expires_at: None,
                rule_id: None,
            };
            store.record_decision(&req, &decision, None, None).await;
        }
        let history = store.history(&AuditFilter::default()).await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].request.id, "p4");
        assert_eq!(history[1].request.id, "p3");
    }

    #[tokio::test]
    async fn install_policy_updates_version() {
        let store = InMemoryPermissionStore::default();
        let before = store.policy().await.version();
        let mut policy = PermissionPolicy::empty();
        policy.read = vec!["src/**".to_string()];
        let after = store.install_policy(policy).await;
        assert_ne!(before, after);
    }
}
