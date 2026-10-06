//! Bind an approval to the invocation that will actually execute.

use crate::orchestration::{PolicyEvaluation, RunApprovalPolicy};
use crate::tool_annotations::ToolAnnotations;
use serde_json::Value;

pub(super) struct DispatchApproval {
    policy: Option<RunApprovalPolicy>,
    session: String,
    initial: (String, Value),
    host_grant: Option<(String, Value)>,
    repeat_count: u64,
}

impl DispatchApproval {
    pub(super) fn new(session: &str, tool: &str, args: &Value) -> Self {
        let policy = crate::orchestration::current_run_approval_policy();
        let repeat_count = if policy.is_some() {
            crate::orchestration::next_approval_policy_repeat_count(session, tool, args)
        } else {
            0
        };
        Self {
            policy,
            session: session.into(),
            initial: (tool.into(), args.clone()),
            host_grant: None,
            repeat_count,
        }
    }

    pub(super) fn evaluate(
        &self,
        annotations: Option<&ToolAnnotations>,
    ) -> Option<PolicyEvaluation> {
        self.policy.as_ref().map(|policy| {
            policy.evaluate_dispatch(
                &self.initial.0,
                &self.initial.1,
                self.repeat_count,
                annotations,
            )
        })
    }

    pub(super) fn record_host_grant(&mut self, tool: &str, args: &Value) {
        self.host_grant = Some((tool.into(), args.clone()));
    }

    pub(super) fn apply_trifecta(
        &self,
        decision: Option<&mut PolicyEvaluation>,
        annotations: Option<&ToolAnnotations>,
    ) {
        let security_policy = crate::security::current_policy();
        let Some(decision) = decision.filter(|decision| decision.is_allow()) else {
            return;
        };
        if !security_policy.trifecta_gate {
            return;
        }
        let taint = crate::llm::agent_session_host::session_taint_snapshot(&self.session);
        if taint.is_empty() {
            return;
        }
        if let Some(outcome) = super::trifecta_gate_reason(
            &security_policy,
            annotations,
            &self.initial.0,
            &self.initial.1,
            &taint,
        ) {
            let extra: &[&str] = if outcome.injection_flagged {
                &["prompt_injection"]
            } else {
                &[]
            };
            super::upgrade_to_trifecta_ask(decision, outcome.reason, extra);
        }
    }

    pub(super) fn recheck(
        &self,
        tool: &str,
        args: &Value,
        annotations: Option<&ToolAnnotations>,
    ) -> Option<PolicyEvaluation> {
        if self.initial.0 == tool && self.initial.1 == *args {
            return None;
        }
        let policy = self.policy.as_ref()?;
        // The same evaluator judges the final facts without consuming another
        // repeat count. An exact host replacement may retain ask approval, but
        // cannot override a hard refusal or approve a later hook/router edit.
        let decision = policy.evaluate_dispatch(tool, args, self.repeat_count, annotations);
        let exact_host_grant = self
            .host_grant
            .as_ref()
            .is_some_and(|(name, granted_args)| name == tool && granted_args == args);
        (decision.is_deny() || (decision.is_ask() && !exact_host_grant)).then_some(decision)
    }
}
