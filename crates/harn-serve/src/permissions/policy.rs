//! Declared permission policy. The shape mirrors the `.harn`
//! `policy { ... }` block on #2503: read/write/exec globs, net host
//! allowlists, an llm provider list with optional cost ceiling, and
//! redaction patterns. Composition is workspace → user → persona, with
//! the *narrowest* scope's rule winning on conflict — same precedence
//! the runtime applies when consulting remember-rules.
//!
//! Policies are versioned by a SHA-256 over their canonical JSON
//! representation, so two policies with the same effective ruleset
//! share a version regardless of formatting / declaration order, and
//! every audit record can pin the version it was decided against.

use std::collections::BTreeSet;
use std::fmt;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::request::ActionClass;

/// Content-hashed identity of a policy. Two policies with the same
/// effective rules — modulo formatting and declaration order — produce
/// the same version. Cheap to copy and embed in audit entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyVersion(String);

impl PolicyVersion {
    /// Sentinel for "no policy declared yet" — used by the in-memory
    /// store on first boot before a real policy is loaded. Decisions
    /// recorded against this version always read as "policy missing"
    /// at audit time.
    pub fn empty() -> Self {
        Self("policy-empty".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PolicyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// LLM-specific policy slice. Providers in `providers` are allowed;
/// any other provider is denied. `cost_ceiling_usd_cents` caps spend
/// per session — `None` means unlimited. The runtime enforces the
/// ceiling via A.11 rate-limit hooks; the store records the ceiling
/// here for audit and so the agent loop can quote it at approval time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmPolicy {
    #[serde(default)]
    pub providers: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_ceiling_usd_cents: Option<u64>,
}

/// Redaction directives. Each entry is a substring or glob the runtime
/// must scrub before persisting to `transcript` (turn record) or
/// `logs` (otel + plain log destinations). Matches behave the same
/// way; the lists exist so a deployment can be louder with logs than
/// with transcripts (transcripts are user-facing; logs go to ops).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionPolicy {
    #[serde(default)]
    pub transcript: Vec<String>,
    #[serde(default)]
    pub logs: Vec<String>,
}

/// The full declared policy for one (workspace, user, persona) scope
/// triple. Higher-precedence scopes override lower-precedence ones
/// through [`PermissionPolicy::compose`], which is associative; the
/// in-memory store composes once at policy load and again whenever a
/// scope is updated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub exec: Vec<String>,
    #[serde(default)]
    pub net: Vec<String>,
    #[serde(default)]
    pub llm: LlmPolicy,
    #[serde(default)]
    pub redact: RedactionPolicy,
    /// Escalation chain for any request not satisfied by an explicit
    /// allow/deny in this policy. Empty means "fall through to a
    /// human." Entries are tried in order.
    #[serde(default)]
    pub escalate_to: Vec<String>,
}

impl PermissionPolicy {
    /// Empty allowlist policy. Every request escalates.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Convenience used by tests and the local-dev profile — allows
    /// everything inside the workspace, blocks network, leaves llm
    /// unrestricted. Mirrors what the TUI ships today as its default
    /// `local-dev` profile.
    pub fn local_dev() -> Self {
        Self {
            read: vec!["**".to_string()],
            write: vec!["**".to_string()],
            exec: vec!["**".to_string()],
            net: vec![],
            llm: LlmPolicy::default(),
            redact: RedactionPolicy::default(),
            escalate_to: vec!["user".to_string()],
        }
    }

    /// Compose `self` with `higher`, where `higher` wins on conflict.
    /// Pattern lists merge (union); the `llm.cost_ceiling_usd_cents`
    /// takes the *tighter* of the two (lower number wins), since a
    /// stricter scope should never be relaxed by a looser one;
    /// `escalate_to` chains higher-first.
    pub fn compose(&self, higher: &PermissionPolicy) -> PermissionPolicy {
        let mut composed = self.clone();
        merge_unique(&mut composed.read, &higher.read);
        merge_unique(&mut composed.write, &higher.write);
        merge_unique(&mut composed.exec, &higher.exec);
        merge_unique(&mut composed.net, &higher.net);
        composed
            .llm
            .providers
            .extend(higher.llm.providers.iter().cloned());
        composed.llm.cost_ceiling_usd_cents = match (
            composed.llm.cost_ceiling_usd_cents,
            higher.llm.cost_ceiling_usd_cents,
        ) {
            (None, other) | (other, None) => other,
            (Some(a), Some(b)) => Some(a.min(b)),
        };
        merge_unique(&mut composed.redact.transcript, &higher.redact.transcript);
        merge_unique(&mut composed.redact.logs, &higher.redact.logs);
        let mut new_chain = higher.escalate_to.clone();
        new_chain.extend(
            composed
                .escalate_to
                .iter()
                .filter(|item| !higher.escalate_to.contains(item))
                .cloned(),
        );
        composed.escalate_to = new_chain;
        composed
    }

    /// Stable content hash. Canonicalizes by sorting list contents
    /// before hashing so policies that declare the same set of rules
    /// in different orders share a version.
    pub fn version(&self) -> PolicyVersion {
        let canonical = self.canonical();
        let serialized = serde_json::to_vec(&canonical).expect("policy serializes");
        let mut hasher = Sha256::new();
        hasher.update(&serialized);
        let digest = hasher.finalize();
        PolicyVersion(format!("policy-{}", hex_short(&digest)))
    }

    /// Reject obviously-broken declarations. Returns the offending
    /// items so callers can render parse-time errors. Glob syntax is
    /// validated against `globset::Glob`; an empty pattern always
    /// fails since it would silently match nothing.
    pub fn lint(&self) -> Result<(), Vec<PolicyLintError>> {
        let mut errors = Vec::new();
        for (label, patterns) in [
            ("read", &self.read),
            ("write", &self.write),
            ("exec", &self.exec),
            ("net", &self.net),
        ] {
            for pattern in patterns {
                if pattern.is_empty() {
                    errors.push(PolicyLintError::EmptyPattern { axis: label });
                    continue;
                }
                if Glob::new(pattern).is_err() {
                    errors.push(PolicyLintError::InvalidGlob {
                        axis: label,
                        pattern: pattern.clone(),
                    });
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Compile the glob slice for one [`ActionClass`]. `Llm` and
    /// `Custom` have no glob slice, so they return an empty matcher;
    /// the store handles them through `llm.providers` /
    /// fall-through-to-escalate instead.
    pub fn matcher_for(&self, class: ActionClass) -> CompiledMatcher {
        let patterns = match class {
            ActionClass::Read => &self.read,
            ActionClass::Write => &self.write,
            ActionClass::Exec => &self.exec,
            ActionClass::Net => &self.net,
            ActionClass::Llm | ActionClass::Custom => return CompiledMatcher::empty(),
        };
        CompiledMatcher::compile(patterns)
    }

    fn canonical(&self) -> CanonicalPolicy {
        let mut policy = self.clone();
        for slice in [
            &mut policy.read,
            &mut policy.write,
            &mut policy.exec,
            &mut policy.net,
        ] {
            slice.sort();
            slice.dedup();
        }
        policy.redact.transcript.sort();
        policy.redact.transcript.dedup();
        policy.redact.logs.sort();
        policy.redact.logs.dedup();
        CanonicalPolicy {
            read: policy.read,
            write: policy.write,
            exec: policy.exec,
            net: policy.net,
            llm_providers: policy.llm.providers,
            llm_cost_ceiling_usd_cents: policy.llm.cost_ceiling_usd_cents,
            redact_transcript: policy.redact.transcript,
            redact_logs: policy.redact.logs,
            escalate_to: policy.escalate_to,
        }
    }
}

fn merge_unique(into: &mut Vec<String>, source: &[String]) {
    for item in source {
        if !into.iter().any(|existing| existing == item) {
            into.push(item.clone());
        }
    }
}

fn hex_short(bytes: &[u8]) -> String {
    bytes.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize)]
struct CanonicalPolicy {
    read: Vec<String>,
    write: Vec<String>,
    exec: Vec<String>,
    net: Vec<String>,
    llm_providers: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    llm_cost_ceiling_usd_cents: Option<u64>,
    redact_transcript: Vec<String>,
    redact_logs: Vec<String>,
    escalate_to: Vec<String>,
}

/// Failure surfaced by [`PermissionPolicy::lint`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyLintError {
    EmptyPattern { axis: &'static str },
    InvalidGlob { axis: &'static str, pattern: String },
}

impl fmt::Display for PolicyLintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyLintError::EmptyPattern { axis } => {
                write!(f, "{axis} policy contains an empty pattern")
            }
            PolicyLintError::InvalidGlob { axis, pattern } => {
                write!(f, "{axis} policy has invalid glob `{pattern}`")
            }
        }
    }
}

/// Compiled glob matcher for one policy axis.
#[derive(Clone)]
pub struct CompiledMatcher {
    set: Option<GlobSet>,
}

impl CompiledMatcher {
    fn empty() -> Self {
        Self { set: None }
    }

    fn compile(patterns: &[String]) -> Self {
        if patterns.is_empty() {
            return Self::empty();
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            if let Ok(glob) = Glob::new(pattern) {
                builder.add(glob);
            }
        }
        builder
            .build()
            .ok()
            .map(|set| Self { set: Some(set) })
            .unwrap_or_else(Self::empty)
    }

    /// `true` when the target matches at least one pattern in the
    /// compiled set. Empty matchers (`Llm`, `Custom`, axes with no
    /// declared patterns) always return `false`.
    pub fn matches(&self, target: &str) -> bool {
        self.set.as_ref().is_some_and(|set| set.is_match(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_policy_matches_nothing() {
        let policy = PermissionPolicy::empty();
        for class in [
            ActionClass::Read,
            ActionClass::Write,
            ActionClass::Exec,
            ActionClass::Net,
        ] {
            assert!(!policy.matcher_for(class).matches("anything"));
        }
    }

    #[test]
    fn local_dev_allows_workspace_globs() {
        let policy = PermissionPolicy::local_dev();
        assert!(policy.matcher_for(ActionClass::Read).matches("src/main.rs"));
        assert!(policy.matcher_for(ActionClass::Write).matches("Cargo.toml"));
        assert!(policy.matcher_for(ActionClass::Exec).matches("ls"));
        assert!(!policy.matcher_for(ActionClass::Net).matches("github.com"));
    }

    #[test]
    fn version_is_stable_under_reordering() {
        let mut a = PermissionPolicy::empty();
        a.read = vec!["src/**".to_string(), "tests/**".to_string()];
        let mut b = PermissionPolicy::empty();
        b.read = vec!["tests/**".to_string(), "src/**".to_string()];
        assert_eq!(a.version(), b.version());
    }

    #[test]
    fn lint_rejects_empty_and_invalid_globs() {
        let mut policy = PermissionPolicy::empty();
        policy.read = vec![String::new(), "src/[".to_string()];
        let errors = policy.lint().expect_err("expected lint errors");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PolicyLintError::EmptyPattern { axis: "read" })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, PolicyLintError::InvalidGlob { axis: "read", .. })));
    }

    #[test]
    fn compose_unions_patterns_and_tightens_cost_ceiling() {
        let mut workspace = PermissionPolicy::empty();
        workspace.read = vec!["src/**".to_string()];
        workspace.llm.cost_ceiling_usd_cents = Some(500);

        let mut user = PermissionPolicy::empty();
        user.read = vec!["tests/**".to_string()];
        user.llm.cost_ceiling_usd_cents = Some(200);

        let composed = workspace.compose(&user);
        assert_eq!(
            composed.read,
            vec!["src/**".to_string(), "tests/**".to_string()]
        );
        assert_eq!(composed.llm.cost_ceiling_usd_cents, Some(200));
    }

    #[test]
    fn compose_chains_escalation_higher_first() {
        let mut workspace = PermissionPolicy::empty();
        workspace.escalate_to = vec!["user".to_string()];
        let mut persona = PermissionPolicy::empty();
        persona.escalate_to = vec!["persona://review-captain".to_string(), "user".to_string()];
        let composed = workspace.compose(&persona);
        assert_eq!(
            composed.escalate_to,
            vec!["persona://review-captain".to_string(), "user".to_string()]
        );
    }
}
