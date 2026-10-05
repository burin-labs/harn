//! Approval evaluation and restrictive composition of declared tool policy.

use super::{
    approval_rules, external_roots, PolicyAction, PolicyEvaluation, ToolApprovalDecision,
    ToolApprovalPolicy,
};

impl ToolApprovalPolicy {
    pub fn evaluate_detailed(&self, tool_name: &str, args: &serde_json::Value) -> PolicyEvaluation {
        approval_rules::evaluate_tool_approval_policy(self, tool_name, args, None)
    }

    pub fn evaluate_detailed_with_repeat(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        repeat_count: u64,
    ) -> PolicyEvaluation {
        approval_rules::evaluate_tool_approval_policy(self, tool_name, args, Some(repeat_count))
    }

    /// Evaluate whether a tool call should be approved, denied, or needs
    /// host confirmation.
    pub fn evaluate(&self, tool_name: &str, args: &serde_json::Value) -> ToolApprovalDecision {
        let decision = self.evaluate_detailed(tool_name, args);
        if decision.is_deny() {
            return ToolApprovalDecision::AutoDenied {
                reason: decision.reason,
            };
        }
        if decision.is_ask() {
            return ToolApprovalDecision::RequiresHostApproval;
        }
        ToolApprovalDecision::AutoApproved
    }

    /// Merge two approval policies, taking the most restrictive combination.
    /// - auto_approve: only tools approved by BOTH policies stay approved
    ///   (if either policy has no patterns, the other's patterns are used)
    /// - auto_deny / require_approval: union (either policy can deny/gate)
    /// - write_path_allowlist: intersection (both must allow the path)
    /// - external_roots: intersection; a shared root keeps the narrower mode
    pub fn intersect(&self, other: &ToolApprovalPolicy) -> ToolApprovalPolicy {
        let auto_approve = if self.auto_approve.is_empty() {
            other.auto_approve.clone()
        } else if other.auto_approve.is_empty() {
            self.auto_approve.clone()
        } else {
            self.auto_approve
                .iter()
                .filter(|p| other.auto_approve.contains(p))
                .cloned()
                .collect()
        };
        let mut auto_deny = self.auto_deny.clone();
        auto_deny.extend(other.auto_deny.iter().cloned());
        let mut require_approval = self.require_approval.clone();
        require_approval.extend(other.require_approval.iter().cloned());
        let write_path_allowlist = if self.write_path_allowlist.is_empty() {
            other.write_path_allowlist.clone()
        } else if other.write_path_allowlist.is_empty() {
            self.write_path_allowlist.clone()
        } else {
            self.write_path_allowlist
                .iter()
                .filter(|p| other.write_path_allowlist.contains(p))
                .cloned()
                .collect()
        };
        let mut rules = self.rules.clone();
        rules.extend(other.rules.iter().cloned());
        let mut sensitive_path_patterns = self.sensitive_path_patterns.clone();
        sensitive_path_patterns.extend(other.sensitive_path_patterns.iter().cloned());
        sensitive_path_patterns.sort();
        sensitive_path_patterns.dedup();
        let external_roots = external_roots::intersect(&self.external_roots, &other.external_roots);
        ToolApprovalPolicy {
            rules,
            auto_approve,
            auto_deny,
            require_approval,
            write_path_allowlist,
            allow_sensitive_paths: self.allow_sensitive_paths && other.allow_sensitive_paths,
            sensitive_path_patterns,
            allow_external_paths: self.allow_external_paths && other.allow_external_paths,
            external_roots,
            repeat_limit: match (self.repeat_limit, other.repeat_limit) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            },
            repeat_action: match (self.repeat_action, other.repeat_action) {
                (Some(PolicyAction::Deny), _) | (_, Some(PolicyAction::Deny)) => {
                    Some(PolicyAction::Deny)
                }
                (Some(PolicyAction::Ask), _) | (_, Some(PolicyAction::Ask)) => {
                    Some(PolicyAction::Ask)
                }
                (Some(PolicyAction::Allow), Some(PolicyAction::Allow)) => Some(PolicyAction::Allow),
                (Some(action), None) | (None, Some(action)) => Some(action),
                (None, None) => None,
            },
        }
    }
}
