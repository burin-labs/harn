//! Bind an approval to the invocation that will actually execute.

use crate::orchestration::{PolicyEvaluation, RunApprovalPolicy};
use crate::tool_annotations::ToolAnnotations;
use serde_json::Value;

pub(super) struct DispatchApproval {
    policy: Option<RunApprovalPolicy>,
    session: String,
    initial: Option<(String, Value)>,
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
        // Unconfigured dispatch captures no approval arguments or strings.
        let initial = policy.as_ref().map(|_| (tool.into(), args.clone()));
        let session = if policy.is_some() {
            session.into()
        } else {
            String::new()
        };
        Self {
            policy,
            session,
            initial,
            host_grant: None,
            repeat_count,
        }
    }

    pub(super) fn evaluate(
        &self,
        annotations: Option<&ToolAnnotations>,
    ) -> Option<PolicyEvaluation> {
        let (tool, args) = self.initial.as_ref()?;
        self.policy
            .as_ref()
            .map(|policy| policy.evaluate_dispatch(tool, args, self.repeat_count, annotations))
    }

    pub(super) fn record_host_grant(&mut self, tool: &str, args: &Value) {
        self.host_grant = Some((tool.into(), args.clone()));
    }

    pub(super) fn apply_trifecta(
        &self,
        decision: Option<&mut PolicyEvaluation>,
        annotations: Option<&ToolAnnotations>,
    ) {
        if let Some((tool, args)) = self.initial.as_ref() {
            self.apply_trifecta_to(decision, annotations, tool, args);
        }
    }

    fn apply_trifecta_to(
        &self,
        decision: Option<&mut PolicyEvaluation>,
        annotations: Option<&ToolAnnotations>,
        tool: &str,
        args: &Value,
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
        if let Some(outcome) =
            super::trifecta_gate_reason(&security_policy, annotations, tool, args, &taint)
        {
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
        tools: Option<&crate::value::VmValue>,
    ) -> Option<PolicyEvaluation> {
        let (initial_tool, initial_args) = self.initial.as_ref()?;
        if initial_tool == tool && initial_args == args {
            return None;
        }
        let policy = self.policy.as_ref()?;
        let annotations = super::tool_catalog::annotations_for(tools, tool);
        // The same evaluator judges the final facts without consuming another
        // repeat count. An exact host replacement may retain ask approval, but
        // cannot override a hard refusal or approve a later hook/router edit.
        let mut decision =
            policy.evaluate_dispatch(tool, args, self.repeat_count, annotations.as_ref());
        self.apply_trifecta_to(Some(&mut decision), annotations.as_ref(), tool, args);
        let exact_host_grant = self
            .host_grant
            .as_ref()
            .is_some_and(|(name, granted_args)| name == tool && granted_args == args);
        (decision.is_deny() || (decision.is_ask() && !exact_host_grant)).then_some(decision)
    }
}
