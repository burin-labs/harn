//! Approval-policy construction for one declared run-authority posture.

use serde::{Deserialize, Serialize};

use super::approval_resolver::ApprovalResolver;

use super::{PolicyAction, ToolApprovalPolicy};

pub fn push_approval_policy(policy: ToolApprovalPolicy) {
    super::EXECUTION_APPROVAL_POLICY_STACK.with(|stack| {
        stack
            .borrow_mut()
            .push(construct_live_approval_policy(policy));
    });
}

pub fn pop_approval_policy() {
    super::EXECUTION_APPROVAL_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

/// Declaration projection for nested scopes and worker configuration. Prepared
/// authority travels separately and is intersected exactly once at dispatch.
pub fn current_approval_policy() -> Option<ToolApprovalPolicy> {
    super::EXECUTION_APPROVAL_POLICY_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map(|policy| policy.declared().clone())
    })
}

/// The typed policy used by live dispatch. Scope transport retains declared
/// configuration, so a reviewer installed later can still answer an ask.
pub fn current_run_approval_policy() -> Option<RunApprovalPolicy> {
    let declared = current_approval_policy();
    if let Some(prepared) = super::PREPARED_APPROVAL_POLICY.with(|slot| slot.borrow().clone()) {
        let policy = declared.map_or_else(
            || prepared.declared().clone(),
            |inner| prepared.declared().intersect(&inner),
        );
        return Some(RunApprovalPolicy::construct_with_resolver(
            prepared.posture(),
            prepared.resolver(),
            |_| policy,
        ));
    }
    declared.map(construct_live_approval_policy)
}

pub(crate) fn construct_live_approval_policy(policy: ToolApprovalPolicy) -> RunApprovalPolicy {
    let available = crate::llm::current_host_bridge().is_some();
    let resolver = if crate::orchestration::current_approval_reviewer().is_some() {
        ApprovalResolver::AutoReview
    } else {
        ApprovalResolver::Host
    };
    RunApprovalPolicy::construct_with_resolver(
        RunAuthorityPosture {
            interactivity: if available {
                RunInteractivity::Interactive
            } else {
                RunInteractivity::NonInteractive
            },
            approval_availability: if available {
                ApprovalAvailability::Available
            } else {
                ApprovalAvailability::Unavailable
            },
            workspace_trust: WorkspaceTrust::Untrusted,
        },
        resolver,
        |_| policy,
    )
}

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
    /// Whether an `ask` on this run has nobody to answer it.
    ///
    /// Takes the resolver, because that is the whole question. A non-interactive
    /// run with no approval bridge genuinely cannot reach a person -- but
    /// "cannot reach a person" and "cannot obtain an answer" stopped being the
    /// same statement once a resolver could answer. Collapsing `Ask` to `Deny`
    /// under a resolver that was installed precisely to answer asks would make
    /// the feature a no-op on headless, which is the only surface the evals run
    /// on.
    fn approval_is_unsatisfiable(self, resolver: ApprovalResolver) -> bool {
        if resolver.answers_ask_without_a_person() {
            return false;
        }
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
    declared: ToolApprovalPolicy,
    resolver: ApprovalResolver,
}

impl RunApprovalPolicy {
    /// Construct with the default [`ApprovalResolver::Host`].
    ///
    /// Kept as-is so every existing caller keeps its exact behavior. Adding the
    /// resolver as a field on [`RunAuthorityPosture`] would have been the
    /// tidier shape and was rejected: it is a public struct with struct-literal
    /// construction across the workspace and one downstream product, so the
    /// field would be a breaking change to a type that is not itself changing.
    pub fn construct(
        posture: RunAuthorityPosture,
        build: impl FnOnce(RunAuthorityPosture) -> ToolApprovalPolicy,
    ) -> Self {
        Self::construct_with_resolver(posture, ApprovalResolver::Host, build)
    }

    /// Construct with an explicit resolver.
    ///
    /// The resolver is host authority and arrives as typed input. It is never
    /// read from ambient config here: a resolver that could be picked up from
    /// the environment would make "which policy did this run enforce" a
    /// question the receipt could not answer.
    pub fn construct_with_resolver(
        posture: RunAuthorityPosture,
        resolver: ApprovalResolver,
        build: impl FnOnce(RunAuthorityPosture) -> ToolApprovalPolicy,
    ) -> Self {
        let declared = build(posture);
        Self {
            posture,
            declared,
            resolver,
        }
    }

    pub fn posture(&self) -> RunAuthorityPosture {
        self.posture
    }

    /// The resolver this run installed.
    pub fn resolver(&self) -> ApprovalResolver {
        self.resolver
    }

    /// The effective evaluator retains the run facts. Use `declared` only to
    /// transport configuration into another scope.
    pub fn effective(&self) -> &Self {
        self
    }

    /// Configuration projection for nested scopes and delegated workers.
    /// Availability is resolved again when the receiving scope dispatches.
    pub fn declared(&self) -> &ToolApprovalPolicy {
        &self.declared
    }

    pub fn evaluate_detailed_with_repeat(
        &self,
        tool: &str,
        args: &serde_json::Value,
        repeat_count: u64,
    ) -> super::PolicyEvaluation {
        let decision = self
            .declared
            .evaluate_detailed_with_repeat(tool, args, repeat_count);
        self.resolve(decision)
    }

    pub fn evaluate_detailed(
        &self,
        tool: &str,
        args: &serde_json::Value,
    ) -> super::PolicyEvaluation {
        self.resolve(self.declared.evaluate_detailed(tool, args))
    }

    pub fn evaluate_request(
        &self,
        request: &super::ToolApprovalRequest,
    ) -> super::PolicyEvaluation {
        self.resolve(self.declared.evaluate_request(request))
    }

    pub fn evaluate(&self, tool: &str, args: &serde_json::Value) -> super::ToolApprovalDecision {
        let decision = self.evaluate_detailed(tool, args);
        if decision.is_deny() {
            super::ToolApprovalDecision::AutoDenied {
                reason: decision.reason,
            }
        } else if decision.is_ask() {
            super::ToolApprovalDecision::RequiresHostApproval
        } else {
            super::ToolApprovalDecision::AutoApproved
        }
    }

    fn resolve(&self, mut decision: super::PolicyEvaluation) -> super::PolicyEvaluation {
        // Select the winning rule before resolving its ask. Rewriting rule
        // actions first can change precedence against an explicit grant.
        if decision.is_ask() && self.posture.approval_is_unsatisfiable(self.resolver) {
            decision.receipt["requested_rule"] = serde_json::json!(decision.matched_rule);
            decision.action = PolicyAction::Deny.as_str().to_string();
            decision.reason = format!("approval unavailable: {}", decision.reason);
            decision.required_approval = None;
            decision.matched_rule = Some(super::PolicyMatchedRule {
                source: "approval_unavailable".to_string(),
                action: PolicyAction::Deny.as_str().to_string(),
                id: decision
                    .matched_rule
                    .as_ref()
                    .and_then(|rule| rule.id.clone()),
                index: decision.matched_rule.as_ref().and_then(|rule| rule.index),
                contributing_rules: Vec::new(),
            });
            decision.receipt["action"] = serde_json::json!(decision.action);
            decision.receipt["reason"] = serde_json::json!(decision.reason);
            decision.receipt["matched_rule"] = serde_json::json!(decision.matched_rule);
            decision.receipt["required_approval"] = serde_json::Value::Null;
        }
        decision
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
    fn unavailable_approval_preserves_grants_denials_and_untouched_calls() {
        let policy: ToolApprovalPolicy = serde_json::from_value(json!({
            "rules": [
                {"source": "mode", "action": "ask", "match": {"tool": "granted"}},
                {"source": "user", "action": "allow", "match": {"tool": "granted"}},
                {"action": "deny", "match": {"tool": "denied"}},
                {"action": "ask", "match": {"tool": "ask"}}
            ]
        }))
        .unwrap();
        let run =
            RunApprovalPolicy::construct(posture(WorkspaceTrust::Untrusted), |_| policy.clone());
        for tool in ["granted", "denied", "untouched"] {
            assert_eq!(
                run.evaluate_detailed(tool, &json!({})),
                policy.evaluate_detailed(tool, &json!({})),
                "{tool}"
            );
        }
        let ask = run.evaluate_detailed("ask", &json!({}));
        assert!(ask.is_deny());
        assert_eq!(
            ask.denial_gate(),
            crate::agent_events::DenialGate::ApprovalUnavailable
        );
        assert_eq!(ask.receipt["requested_rule"]["action"], "ask");
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

    /// THE MATCHED PAIR for the conditional collapse.
    ///
    /// The same posture -- non-interactive, no approval bridge -- and the same
    /// `ask` rule, resolved two ways. Under `Host` the ask has nobody to answer
    /// it and must still collapse to a denial; under a resolver that answers,
    /// it must survive as an ask. Either test alone proves nothing: the first
    /// passes against code that always collapses, the second against code that
    /// never does.
    fn ask_rule_policy() -> ToolApprovalPolicy {
        serde_json::from_value(json!({
            "rules": [{
                "id": "rule-ask",
                "action": "ask",
                "match": {"tool": "rule_tool"},
                "reason": "review the rule tool"
            }]
        }))
        .expect("policy parses")
    }

    #[test]
    fn host_resolver_still_collapses_an_unanswerable_ask() {
        let policy = RunApprovalPolicy::construct_with_resolver(
            posture(WorkspaceTrust::Trusted),
            ApprovalResolver::Host,
            |_| ask_rule_policy(),
        );
        assert!(matches!(
            policy.effective().evaluate("rule_tool", &json!({})),
            super::super::ToolApprovalDecision::AutoDenied { .. }
        ));
        assert_eq!(policy.resolver(), ApprovalResolver::Host);
    }

    #[test]
    fn a_resolver_that_answers_keeps_the_ask_askable() {
        for resolver in [ApprovalResolver::AutoReview, ApprovalResolver::AllowAll] {
            let policy = RunApprovalPolicy::construct_with_resolver(
                posture(WorkspaceTrust::Trusted),
                resolver,
                |_| ask_rule_policy(),
            );
            // Not AutoDenied. Collapsing here is what would make the whole
            // feature a no-op on headless -- the only surface the evals run on.
            assert_eq!(
                policy.effective().evaluate("rule_tool", &json!({})),
                super::super::ToolApprovalDecision::RequiresHostApproval,
                "{resolver:?} answers asks, so the ask must reach it"
            );
            assert_eq!(policy.resolver(), resolver);
        }
    }

    #[test]
    fn the_default_constructor_is_unchanged() {
        // Every existing caller must keep its exact behavior, or this becomes a
        // silent auto-approver rollout rather than an opt-in one.
        let legacy =
            RunApprovalPolicy::construct(posture(WorkspaceTrust::Trusted), |_| ask_rule_policy());
        assert_eq!(legacy.resolver(), ApprovalResolver::Host);
        assert!(matches!(
            legacy.effective().evaluate("rule_tool", &json!({})),
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
