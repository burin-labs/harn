//! Persistent "remember this answer" rules. A rule pins one action +
//! target glob to a fixed verdict at a chosen [`DecisionScope`], with
//! an optional `expires_at` for time-bound grants.
//!
//! Rules are intentionally cheap to evaluate: each one carries a
//! pre-compiled glob matcher, and the store walks them narrowest-scope
//! first so a session-scoped deny short-circuits a workspace-scoped
//! "always allow."

use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::request::{ActionClass, DecisionScope, PermissionRequest};

/// Stable identifier for a rule. Rendered as a UUIDv7 so the prefix is
/// time-ordered — useful when sorting rules at audit time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl RuleId {
    pub fn new() -> Self {
        Self(format!("rule_{}", Uuid::now_v7()))
    }
}

impl Default for RuleId {
    fn default() -> Self {
        Self::new()
    }
}

/// One persistent rule. `target_pattern` is a glob compiled once at
/// rule-load and re-used on every check. The matcher is cached
/// separately from the serialized field so the rule round-trips
/// through JSON without losing the compile.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RememberRule {
    pub id: RuleId,
    pub tenant_id: Option<String>,
    pub scope: DecisionScope,
    /// Identifier matching the request's scope. For
    /// `DecisionScope::Session` this is the session id; for
    /// `Workspace` the workspace id; for `User` the actor; for
    /// `Always` it's ignored (rule applies everywhere).
    pub scope_value: Option<String>,
    pub class: ActionClass,
    /// Glob matched against `PermissionRequest::action`. Use `*` to
    /// match every action of the class.
    pub action_pattern: String,
    /// Glob matched against `PermissionRequest::target`. Use `**` to
    /// match any target.
    pub target_pattern: String,
    pub allow: bool,
    pub reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub created_by: String,
    #[serde(default)]
    pub expires_at: Option<OffsetDateTime>,
    /// Soft-revoked rules are kept for audit but skipped during
    /// evaluation. Hard-revoke removes the row entirely; soft-revoke
    /// is the preferred path so "why did this rule stop applying" is
    /// answerable from the rule log.
    #[serde(default)]
    pub revoked_at: Option<OffsetDateTime>,
    /// Pre-compiled matchers, populated by `Self::compile`. Not
    /// serialized; rebuilt on load via [`RememberRule::ensure_compiled`].
    #[serde(skip)]
    compiled: Option<CompiledRule>,
}

#[derive(Clone)]
struct CompiledRule {
    action: GlobMatcher,
    target: GlobMatcher,
}

impl std::fmt::Debug for CompiledRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRule")
            .field("action", &"<glob>")
            .field("target", &"<glob>")
            .finish()
    }
}

impl RememberRule {
    pub fn new(
        scope: DecisionScope,
        scope_value: Option<String>,
        class: ActionClass,
        action_pattern: impl Into<String>,
        target_pattern: impl Into<String>,
        allow: bool,
        created_by: impl Into<String>,
    ) -> Result<Self, RuleCompileError> {
        let mut rule = Self {
            id: RuleId::new(),
            tenant_id: None,
            scope,
            scope_value,
            class,
            action_pattern: action_pattern.into(),
            target_pattern: target_pattern.into(),
            allow,
            reason: None,
            created_at: OffsetDateTime::now_utc(),
            created_by: created_by.into(),
            expires_at: None,
            revoked_at: None,
            compiled: None,
        };
        rule.ensure_compiled()?;
        Ok(rule)
    }

    /// Compile the action + target patterns. Called automatically by
    /// `new`; load paths (deserialization from a backing store) must
    /// call this before the rule is consulted.
    pub fn ensure_compiled(&mut self) -> Result<(), RuleCompileError> {
        if self.compiled.is_some() {
            return Ok(());
        }
        let action = Glob::new(&self.action_pattern)
            .map_err(|err| RuleCompileError::Action(err.to_string()))?
            .compile_matcher();
        let target = Glob::new(&self.target_pattern)
            .map_err(|err| RuleCompileError::Target(err.to_string()))?
            .compile_matcher();
        self.compiled = Some(CompiledRule { action, target });
        Ok(())
    }

    /// `true` when the rule applies to `request` given `now`. Walks
    /// every dimension the store cares about: tenant match, scope
    /// match, action class, action glob, target glob, revocation,
    /// expiry.
    pub fn matches(&self, request: &PermissionRequest, now: OffsetDateTime) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        if let Some(expires_at) = self.expires_at {
            if expires_at <= now {
                return false;
            }
        }
        if !tenants_match(self.tenant_id.as_deref(), request.tenant_id.as_deref()) {
            return false;
        }
        if !scope_matches(self.scope, self.scope_value.as_deref(), request) {
            return false;
        }
        if self.class != request.class {
            return false;
        }
        let Some(compiled) = self.compiled.as_ref() else {
            return false;
        };
        compiled.action.is_match(&request.action) && compiled.target.is_match(&request.target)
    }
}

fn tenants_match(rule_tenant: Option<&str>, request_tenant: Option<&str>) -> bool {
    match (rule_tenant, request_tenant) {
        (None, _) => true,
        (Some(rule), Some(req)) => rule == req,
        (Some(_), None) => false,
    }
}

fn scope_matches(
    scope: DecisionScope,
    scope_value: Option<&str>,
    request: &PermissionRequest,
) -> bool {
    match scope {
        DecisionScope::Always => true,
        DecisionScope::Session => scope_value == Some(request.session_id.as_str()),
        DecisionScope::Workspace => {
            scope_value.is_some() && scope_value == request.workspace_id.as_deref()
        }
        DecisionScope::User => scope_value == Some(request.actor.as_str()),
    }
}

/// Failure parsing one of the patterns when constructing a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleCompileError {
    Action(String),
    Target(String),
}

impl std::fmt::Display for RuleCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleCompileError::Action(msg) => write!(f, "invalid action pattern: {msg}"),
            RuleCompileError::Target(msg) => write!(f, "invalid target pattern: {msg}"),
        }
    }
}

impl std::error::Error for RuleCompileError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(session: &str, actor: &str, action: &str, target: &str) -> PermissionRequest {
        let mut req =
            PermissionRequest::new("p1", session, actor, ActionClass::Read, action, target);
        req.workspace_id = Some("ws".to_string());
        req
    }

    #[test]
    fn session_scoped_rule_matches_session_id_only() {
        let mut rule = RememberRule::new(
            DecisionScope::Session,
            Some("s1".to_string()),
            ActionClass::Read,
            "fs.*",
            "src/**",
            true,
            "alice",
        )
        .unwrap();
        rule.ensure_compiled().unwrap();
        let now = OffsetDateTime::now_utc();
        assert!(rule.matches(&request("s1", "alice", "fs.read", "src/lib.rs"), now));
        assert!(!rule.matches(&request("s2", "alice", "fs.read", "src/lib.rs"), now));
    }

    #[test]
    fn always_scope_matches_any_session_or_actor() {
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
        let now = OffsetDateTime::now_utc();
        assert!(rule.matches(&request("any", "bob", "fs.read", "src/lib.rs"), now));
    }

    #[test]
    fn expired_rule_does_not_match() {
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
        let past = OffsetDateTime::now_utc() - time::Duration::minutes(5);
        rule.expires_at = Some(past);
        assert!(!rule.matches(
            &request("s", "a", "fs.read", "x"),
            OffsetDateTime::now_utc()
        ));
    }

    #[test]
    fn soft_revoked_rule_does_not_match() {
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
        rule.revoked_at = Some(OffsetDateTime::now_utc());
        assert!(!rule.matches(
            &request("s", "a", "fs.read", "x"),
            OffsetDateTime::now_utc()
        ));
    }

    #[test]
    fn workspace_scope_requires_workspace_id_match() {
        let mut rule = RememberRule::new(
            DecisionScope::Workspace,
            Some("ws".to_string()),
            ActionClass::Read,
            "*",
            "**",
            true,
            "alice",
        )
        .unwrap();
        rule.ensure_compiled().unwrap();
        let now = OffsetDateTime::now_utc();
        let req = request("s1", "alice", "fs.read", "src/x");
        assert!(rule.matches(&req, now));
        let mut other = req.clone();
        other.workspace_id = Some("ws-other".to_string());
        assert!(!rule.matches(&other, now));
    }

    #[test]
    fn ensure_compiled_is_idempotent() {
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
        rule.ensure_compiled().unwrap();
        rule.ensure_compiled().unwrap();
    }
}
