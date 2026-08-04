use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::{
    any_glob_matches, evaluate_tool_approval_request, EvaluationContext, PolicyAction,
    PolicyEvaluation, PolicyRule, ToolApprovalPolicy,
};

/// A host-facing tool approval request.
///
/// Native hosts pass the raw Harn permission receipt fields through this
/// interface. The evaluator owns normalization of `policy_decision.context`
/// and compatibility aliases, so callers never reconstruct matcher inputs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolApprovalRequest {
    pub tool_name: String,
    pub arguments: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_decision: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_request: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_count: Option<u64>,
}

impl ToolApprovalPolicy {
    /// Evaluate a raw host request through the same normalization, guards,
    /// precedence, and audit receipt used by VM tool dispatch.
    pub fn evaluate_request(&self, request: &ToolApprovalRequest) -> PolicyEvaluation {
        evaluate_tool_approval_request(self, request)
    }
}

fn normalized_env_modes(env_modes: &[String]) -> Vec<String> {
    if env_modes.is_empty() {
        vec!["inherit_clean".to_string()]
    } else {
        env_modes.to_vec()
    }
}

pub(super) fn env_modes_match(patterns: &[String], env_modes: &[String]) -> bool {
    patterns.is_empty()
        || normalized_env_modes(env_modes)
            .iter()
            .all(|mode| any_glob_matches(patterns, std::slice::from_ref(mode)))
}

pub(super) fn exact_write_env_allow(rule: &PolicyRule, ctx: &EvaluationContext) -> bool {
    if rule.action != PolicyAction::Allow {
        return true;
    }
    normalized_env_modes(&ctx.env_modes)
        .iter()
        .filter(|mode| matches!(mode.as_str(), "patch" | "replace"))
        .all(|mode| rule.matches.env_mode.contains(mode))
}
