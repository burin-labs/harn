//! The built-in guards, and the gate each of their refusals belongs to.
//!
//! These run BEFORE any configured approval rule is consulted, and they
//! answer a different question: is this declared path one the session may
//! name at all. That answer does not change when a person turns approval
//! off. Keeping the guards and the source-to-gate mapping in one module is
//! what stops the two drifting apart, which is how every refusal came to
//! report itself as an approval decision.

use super::*;

/// The deny-by-default sensitive-path guard.
pub const SOURCE_DEFAULT_SENSITIVE_PATH: &str = "default_sensitive_path";
/// The workspace path boundary refusing a malformed declared path.
pub const SOURCE_DEFAULT_PATH_GUARD: &str = "default_path_guard";
/// The workspace path boundary refusing a path outside every admitted root.
pub const SOURCE_DEFAULT_EXTERNAL_PATH: &str = "default_external_path";

/// Which refusing mechanism a deciding rule belongs to.
///
/// Three of the sources this module produces are not approval decisions.
/// [`default_guard`] runs BEFORE any configured rule is consulted and answers
/// a scope question: is this declared path one the session may name at all.
/// That answer does not change when a person turns approval off, so reporting
/// it under the approval gate sends the reader to inspect a control that had
/// no part in the refusal.
///
/// The mapping is owned here, beside the rules that produce those sources,
/// rather than at the dispatch boundary that consumes it. A boundary that
/// re-derived the gate from the reason prose, or pinned one value the way the
/// dispatch seam used to, can name a gate the rule never chose.
pub fn denial_gate_for_source(source: Option<&str>) -> crate::agent_events::DenialGate {
    use crate::agent_events::DenialGate;
    match source {
        Some(SOURCE_DEFAULT_SENSITIVE_PATH) => DenialGate::SensitivePath,
        Some(SOURCE_DEFAULT_PATH_GUARD) | Some(SOURCE_DEFAULT_EXTERNAL_PATH) => {
            DenialGate::WorkspaceBoundary
        }
        // Every other source IS a configured approval decision: an
        // `auto_deny` pattern, a policy rule, the write-path allowlist, or
        // the repeat limit. So is a deny carrying no matched rule at all,
        // which only a future rule-less refusal could produce.
        _ => DenialGate::ApprovalPolicy,
    }
}

pub(super) fn default_guard(
    policy: &ToolApprovalPolicy,
    ctx: &EvaluationContext,
) -> Option<Candidate> {
    if !policy.allow_sensitive_paths {
        if let Some(path) = sensitive_paths::first_candidate(policy, &ctx.path_candidates) {
            let path = sensitive_paths::bounded_evidence(&path);
            return Some(Candidate {
                source: SOURCE_DEFAULT_SENSITIVE_PATH.to_string(),
                index: None,
                id: Some("sensitive_path".to_string()),
                action: PolicyAction::Deny,
                reason: format!("path '{path}' is denied by the sensitive-path default"),
                approval: ApprovalShape::default(),
                risk_labels: vec!["sensitive_path".to_string()],
                denied_paths: vec![path],
            });
        }
    }

    if !policy.allow_external_paths {
        for entry in &ctx.path_entries {
            if matches!(entry.kind, WorkspacePathKind::Invalid) {
                return Some(Candidate {
                    source: SOURCE_DEFAULT_PATH_GUARD.to_string(),
                    index: None,
                    id: Some("invalid_path".to_string()),
                    action: PolicyAction::Deny,
                    reason: entry
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("path '{}' is invalid", entry.display_path())),
                    approval: ApprovalShape::default(),
                    risk_labels: vec!["invalid_path".to_string()],
                    denied_paths: vec![entry.display_path().to_string()],
                });
            }
            if entry.workspace_path.is_none()
                && entry
                    .host_path
                    .as_ref()
                    .is_some_and(|path| !under_external_root(path, &policy.external_roots))
            {
                return Some(Candidate {
                    source: SOURCE_DEFAULT_EXTERNAL_PATH.to_string(),
                    index: None,
                    id: Some("external_path".to_string()),
                    action: PolicyAction::Deny,
                    reason: format!(
                        "path '{}' is outside the workspace and no external root allows it",
                        entry.display_path()
                    ),
                    approval: ApprovalShape::default(),
                    risk_labels: vec!["external_path".to_string()],
                    denied_paths: vec![entry.display_path().to_string()],
                });
            }
        }
    }

    None
}

impl PolicyEvaluation {
    /// The gate a refusal from this decision belongs to.
    ///
    /// Reads the rule that actually decided rather than assuming one gate for
    /// the whole evaluator, so the built-in path guards do not report
    /// themselves as approval decisions. See [`denial_gate_for_source`].
    pub fn denial_gate(&self) -> crate::agent_events::DenialGate {
        denial_gate_for_source(self.matched_rule.as_ref().map(|rule| rule.source.as_str()))
    }

    /// Turn this refusal into the terminal denial a dispatch seam reports.
    ///
    /// The seam gets no gate argument on purpose. Every caller that could
    /// name one could name the wrong one, and one of them did: the tool
    /// dispatch path passed a literal `ApprovalPolicy` for every refusal this
    /// evaluator produced, so a boundary refusal announced itself as an
    /// approval denial to a reader who had approval switched off. Keeping the
    /// gate un-nameable at the call site is what stops that recurring in the
    /// next seam rather than only in the one that was caught.
    ///
    /// Borrows rather than consumes because the same refusal still has to
    /// supply its receipt to the denial evidence alongside this.
    pub fn terminal_denial(&self) -> crate::agent_events::ToolDenial {
        let mut denial = crate::agent_events::ToolDenial::terminal(
            self.denial_gate(),
            None,
            self.reason.clone(),
        );
        denial.denied_paths = self.denied_paths.clone();
        denial
    }
}
