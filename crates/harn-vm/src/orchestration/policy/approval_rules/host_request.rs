use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::{
    evaluate_tool_approval_request, EvaluationContext, PolicyAction, PolicyEvaluation,
    PolicyIdentityMatch, PolicyRule, ToolApprovalPolicy,
};

/// A host-facing tool approval request.
///
/// Native hosts pass the raw Harn permission receipt fields through this
/// interface. The evaluator owns normalization of `policy_decision.context`
/// and compatibility aliases, so callers never reconstruct matcher inputs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl ToolApprovalRequest {
    /// Parse explicit native-host input without defaulting an absent request to allow.
    pub fn from_host_json(value: JsonValue) -> Result<Self, String> {
        if !value.is_object() {
            return Err("request must be an object".into());
        }
        let request: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if request.tool_name.trim().is_empty() {
            return Err("tool_name must be nonempty".into());
        }
        if !request.arguments.is_object() {
            return Err("arguments must be an object".into());
        }
        if request
            .policy_decision
            .as_ref()
            .is_some_and(|value| !value.is_object())
            || request
                .approval_request
                .as_ref()
                .is_some_and(|value| !value.is_object())
        {
            return Err("policy_decision and approval_request must be objects when present".into());
        }
        Ok(request)
    }
}

impl ToolApprovalPolicy {
    /// Decode explicit host input without silently dropping misspelled policy fields.
    pub fn from_host_json(value: JsonValue) -> Result<Self, String> {
        if !value.is_object() {
            return Err("policy must be an object".into());
        }
        let mut ignored = Vec::new();
        let policy = serde_ignored::deserialize(value, |path| ignored.push(path.to_string()))
            .map_err(|error| error.to_string())?;
        if !ignored.is_empty() {
            return Err(format!("unknown policy fields: {}", ignored.join(", ")));
        }
        Ok(policy)
    }

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

pub(super) fn env_modes_match(
    patterns: &[String],
    env_modes: &[String],
    identity: PolicyIdentityMatch,
) -> bool {
    patterns.is_empty()
        || normalized_env_modes(env_modes)
            .iter()
            .all(|mode| identity.matches(patterns, std::slice::from_ref(mode)))
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
