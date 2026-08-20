//! Approval-policy construction for one declared run-authority posture.

use serde::{Deserialize, Serialize};

use super::{PolicyAction, ToolApprovalPolicy};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInteractivity {
    Interactive,
    NonInteractive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAvailability {
    Available,
    Unavailable,
}

/// Why the host permits or refuses project-scoped workspace policy.
///
/// `HostMaterialized` is deliberately distinct from durable user trust. It
/// lets CI, eval, scheduled, and hosted adapters declare that they created the
/// run's isolated workspace without adding disposable paths to a user trust
/// store or recognizing one product's directory layout inside Harn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrust {
    Untrusted,
    Trusted,
    HostMaterialized,
}

impl WorkspaceTrust {
    pub fn permits_project_policy(self) -> bool {
        matches!(self, Self::Trusted | Self::HostMaterialized)
    }
}

/// Facts that policy construction must know before a run starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAuthorityPosture {
    pub interactivity: RunInteractivity,
    pub approval_availability: ApprovalAvailability,
    pub workspace_trust: WorkspaceTrust,
}

impl RunAuthorityPosture {
    fn approval_is_unsatisfiable(self) -> bool {
        self.interactivity == RunInteractivity::NonInteractive
            && self.approval_availability == ApprovalAvailability::Unavailable
    }
}

/// A tool-approval policy constructed with the run facts that determine
/// whether approval and workspace trust are usable.
///
/// The fields stay private so `PreparedRun` cannot once again receive a policy
/// and posture assembled independently. Hosts construct this value through
/// [`RunApprovalPolicy::construct`], where their adapter can select trust
/// layers from the typed posture. Harn then resolves every legacy form of an
/// unsatisfiable `ask` to a deterministic denial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunApprovalPolicy {
    posture: RunAuthorityPosture,
    effective: ToolApprovalPolicy,
}

impl RunApprovalPolicy {
    pub fn construct(
        posture: RunAuthorityPosture,
        build: impl FnOnce(RunAuthorityPosture) -> ToolApprovalPolicy,
    ) -> Self {
        let mut effective = build(posture);
        if posture.approval_is_unsatisfiable() {
            deny_unsatisfiable_approval(&mut effective);
        }
        Self { posture, effective }
    }

    pub fn posture(&self) -> RunAuthorityPosture {
        self.posture
    }

    pub fn effective(&self) -> &ToolApprovalPolicy {
        &self.effective
    }
}

fn deny_unsatisfiable_approval(policy: &mut ToolApprovalPolicy) {
    for rule in &mut policy.rules {
        if rule.action == PolicyAction::Ask {
            rule.action = PolicyAction::Deny;
            rule.reason = Some(match rule.reason.take() {
                Some(reason) => format!("approval unavailable: {reason}"),
                None => "approval unavailable: the matched rule requires approval".to_string(),
            });
        }
    }

    for pattern in std::mem::take(&mut policy.require_approval) {
        if !policy.auto_deny.contains(&pattern) {
            policy.auto_deny.push(pattern);
        }
    }

    if policy.repeat_limit.is_some()
        && matches!(policy.repeat_action, None | Some(PolicyAction::Ask))
    {
        policy.repeat_action = Some(PolicyAction::Deny);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn posture(workspace_trust: WorkspaceTrust) -> RunAuthorityPosture {
        RunAuthorityPosture {
            interactivity: RunInteractivity::NonInteractive,
            approval_availability: ApprovalAvailability::Unavailable,
            workspace_trust,
        }
    }

    #[test]
    fn host_materialized_workspace_reaches_policy_construction() {
        let materialized =
            RunApprovalPolicy::construct(posture(WorkspaceTrust::HostMaterialized), |posture| {
                ToolApprovalPolicy {
                    auto_deny: (!posture.workspace_trust.permits_project_policy())
                        .then(|| "edit".to_string())
                        .into_iter()
                        .collect(),
                    ..ToolApprovalPolicy::default()
                }
            });
        let untrusted =
            RunApprovalPolicy::construct(posture(WorkspaceTrust::Untrusted), |posture| {
                ToolApprovalPolicy {
                    auto_deny: (!posture.workspace_trust.permits_project_policy())
                        .then(|| "edit".to_string())
                        .into_iter()
                        .collect(),
                    ..ToolApprovalPolicy::default()
                }
            });

        assert_eq!(
            materialized.effective().evaluate("edit", &json!({})),
            super::super::ToolApprovalDecision::AutoApproved
        );
        assert!(matches!(
            untrusted.effective().evaluate("edit", &json!({})),
            super::super::ToolApprovalDecision::AutoDenied { .. }
        ));
    }

    #[test]
    fn unavailable_noninteractive_policy_has_no_satisfiable_ask_form() {
        let policy = RunApprovalPolicy::construct(posture(WorkspaceTrust::Trusted), |_| {
            serde_json::from_value(json!({
                "rules": [{
                    "id": "rule-ask",
                    "action": "ask",
                    "match": {"tool": "rule_tool"},
                    "reason": "review the rule tool"
                }],
                "require_approval": ["legacy_tool"],
                "repeat_limit": 1
            }))
            .expect("policy")
        });

        for (tool, repeat_count) in [("rule_tool", 0), ("legacy_tool", 0), ("other", 2)] {
            let decision =
                policy
                    .effective()
                    .evaluate_detailed_with_repeat(tool, &json!({}), repeat_count);
            assert!(decision.is_deny(), "{tool} remained {:?}", decision.action);
            assert!(
                !decision.is_ask(),
                "{tool} still requires unavailable approval"
            );
        }
    }

    #[test]
    fn other_postures_preserve_reviewable_asks() {
        for posture in [
            RunAuthorityPosture {
                interactivity: RunInteractivity::Interactive,
                approval_availability: ApprovalAvailability::Unavailable,
                workspace_trust: WorkspaceTrust::Trusted,
            },
            RunAuthorityPosture {
                interactivity: RunInteractivity::NonInteractive,
                approval_availability: ApprovalAvailability::Available,
                workspace_trust: WorkspaceTrust::Trusted,
            },
        ] {
            let policy = RunApprovalPolicy::construct(posture, |_| ToolApprovalPolicy {
                require_approval: vec!["edit".to_string()],
                ..ToolApprovalPolicy::default()
            });
            assert!(policy
                .effective()
                .evaluate_detailed("edit", &json!({}))
                .is_ask());
        }
    }
}
