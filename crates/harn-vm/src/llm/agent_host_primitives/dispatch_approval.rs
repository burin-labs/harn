//! Bind an approval to the invocation that will actually execute.

use crate::orchestration::{PolicyEvaluation, RunApprovalPolicy};
use crate::tool_annotations::ToolAnnotations;
use serde_json::Value;

struct InvocationBinding {
    tool: String,
    args: Value,
    identity: Option<String>,
    ask_risks: Option<Vec<String>>,
}

impl InvocationBinding {
    fn new(tool: &str, args: &Value) -> Self {
        Self {
            tool: tool.into(),
            args: args.clone(),
            identity: None,
            ask_risks: None,
        }
    }

    fn permits_ask(
        &self,
        tool: &str,
        args: &Value,
        identity: &Option<String>,
        decision: &PolicyEvaluation,
    ) -> bool {
        self.tool == tool
            && self.args == *args
            && self
                .identity
                .as_ref()
                .zip(identity.as_ref())
                .is_some_and(|(approved, current)| approved == current)
            && self
                .ask_risks
                .as_ref()
                .is_some_and(|risks| risks == &decision.risk_labels)
    }
}

pub(super) struct DispatchApproval {
    policy: Option<RunApprovalPolicy>,
    session: String,
    initial: Option<InvocationBinding>,
    host_grant: Option<InvocationBinding>,
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
        let initial = policy.as_ref().map(|_| InvocationBinding::new(tool, args));
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
        &mut self,
        annotations: Option<&ToolAnnotations>,
    ) -> Option<PolicyEvaluation> {
        let initial = self.initial.as_mut()?;
        let (decision, identity) = self.policy.as_ref()?.evaluate_dispatch(
            &initial.tool,
            &initial.args,
            self.repeat_count,
            annotations,
        );
        initial.identity = identity;
        Some(decision)
    }

    pub(super) fn record_host_grant(
        &mut self,
        tool: &str,
        args: &Value,
        annotations: Option<&ToolAnnotations>,
    ) {
        let Some(policy) = self.policy.as_ref() else {
            return;
        };
        let (mut decision, identity) =
            policy.evaluate_dispatch(tool, args, self.repeat_count, annotations);
        self.apply_trifecta_to(Some(&mut decision), annotations, tool, args);
        let mut grant = InvocationBinding::new(tool, args);
        grant.identity = identity;
        grant.ask_risks = decision.is_ask().then(|| decision.risk_labels.clone());
        self.host_grant = Some(grant);
    }

    pub(super) fn apply_trifecta(
        &mut self,
        mut decision: Option<&mut PolicyEvaluation>,
        annotations: Option<&ToolAnnotations>,
    ) {
        if let Some(initial) = self.initial.as_ref() {
            self.apply_trifecta_to(
                decision.as_deref_mut(),
                annotations,
                &initial.tool,
                &initial.args,
            );
        }
        if let Some(initial) = self.initial.as_mut() {
            initial.ask_risks = decision
                .filter(|decision| decision.is_ask())
                .map(|decision| decision.risk_labels.clone());
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
        annotations: Option<&ToolAnnotations>,
    ) -> Option<PolicyEvaluation> {
        let initial = self.initial.as_ref()?;
        let policy = self.policy.as_ref()?;
        // Reuse the complete evaluator and its canonical identity. Identical
        // JSON cannot retain a grant when resolved resources or risk change.
        let (mut decision, identity) =
            policy.evaluate_dispatch(tool, args, self.repeat_count, annotations);
        self.apply_trifecta_to(Some(&mut decision), annotations, tool, args);
        let exact_grant = initial.permits_ask(tool, args, &identity, &decision)
            || self
                .host_grant
                .as_ref()
                .is_some_and(|grant| grant.permits_ask(tool, args, &identity, &decision));
        (decision.is_deny() || (decision.is_ask() && !exact_grant)).then_some(decision)
    }
}
