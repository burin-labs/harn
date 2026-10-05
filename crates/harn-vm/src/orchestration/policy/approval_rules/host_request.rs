use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::Path;

use super::{
    evaluate_tool_approval_request, first_string, EvaluationContext, PolicyAction,
    PolicyEvaluation, PolicyIdentityMatch, PolicyRule, ToolApprovalPolicy,
};

/// Workspace authority supplied by the host that owns the project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolApprovalWorkspaceBoundary {
    pub root: String,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_boundary: Option<ToolApprovalWorkspaceBoundary>,
    /// The canonical Harn tool catalog entry, projected without a host registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_annotations: Option<crate::tool_annotations::ToolAnnotations>,
}

impl ToolApprovalRequest {
    /// Parse explicit native-host input without defaulting an absent request to allow.
    pub fn from_host_json(value: JsonValue) -> Result<Self, String> {
        if !value.is_object() {
            return Err("request must be an object".into());
        }
        let mut ignored = Vec::new();
        let request: Self =
            serde_ignored::deserialize(value, |path| ignored.push(path.to_string()))
                .map_err(|error| error.to_string())?;
        if !ignored.is_empty() {
            return Err(format!("unknown request fields: {}", ignored.join(", ")));
        }
        request.validate()?;
        Ok(request)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.tool_name.trim().is_empty() {
            return Err("tool_name must be nonempty".into());
        }
        if !self.arguments.is_object() {
            return Err("arguments must be an object".into());
        }
        if self.tool_annotations.is_some() && self.workspace_boundary.is_none() {
            return Err("tool_annotations require an explicit workspace_boundary".into());
        }
        if self
            .policy_decision
            .as_ref()
            .is_some_and(|value| !value.is_object())
            || self
                .approval_request
                .as_ref()
                .is_some_and(|value| !value.is_object())
        {
            return Err("policy_decision and approval_request must be objects when present".into());
        }
        if let Some(boundary) = &self.workspace_boundary {
            if boundary.root.contains('\0')
                || !Path::new(&boundary.root).is_absolute()
                || !Path::new(&boundary.root).is_dir()
            {
                return Err(
                    "workspace_boundary.root must name an existing absolute directory".into(),
                );
            }
            let annotations = self
                .tool_annotations
                .clone()
                .or_else(|| super::super::current_tool_annotations(&self.tool_name));
            let params = annotations
                .map(|value| value.arg_schema.path_params)
                .unwrap_or_else(|| {
                    [
                        "path",
                        "file",
                        "target",
                        "source_path",
                        "new_path",
                        "target_path",
                        "paths",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                });
            {
                for param in &params {
                    if param.trim().is_empty() {
                        return Err(
                            "tool_annotations.arg_schema.path_params must contain nonempty field names"
                                .into(),
                        );
                    }
                    match self.arguments.get(param) {
                        None => {}
                        Some(JsonValue::String(value)) if !value.trim().is_empty() => {}
                        Some(JsonValue::Array(values))
                            if values.iter().all(|value| {
                                value.as_str().is_some_and(|value| !value.trim().is_empty())
                            }) => {}
                        _ => {
                            return Err(format!(
                            "workspace path argument '{param}' must be a string or list of strings"
                        ))
                        }
                    }
                }
            }
        }
        Ok(())
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

pub(super) fn invalid_request(request: &ToolApprovalRequest, reason: String) -> PolicyEvaluation {
    let context =
        EvaluationContext::new(&request.tool_name, &request.arguments, request.repeat_count);
    super::evaluation_from_candidate(
        super::Candidate {
            source: "invalid_host_request".into(),
            source_rank: super::PolicyRuleSource::Policy,
            index: None,
            id: Some("invalid_host_request".into()),
            action: PolicyAction::Deny,
            reason,
            approval: Default::default(),
            risk_labels: vec!["invalid_host_request".into()],
            denied_paths: Vec::new(),
            contributing_rules: Vec::new(),
        },
        &context,
    )
}

pub(super) fn request_context(request: &ToolApprovalRequest) -> EvaluationContext {
    let mut context =
        EvaluationContext::new(&request.tool_name, &request.arguments, request.repeat_count);
    let declared_params = request
        .tool_annotations
        .as_ref()
        .map(|value| value.arg_schema.path_params.as_slice());
    let owned_paths = request.workspace_boundary.as_ref().map(|boundary| {
        let mut paths = if declared_params.is_some() {
            Vec::new()
        } else {
            context
                .path_entries
                .iter()
                .map(|entry| entry.input.clone())
                .collect::<Vec<_>>()
        };
        let params =
            declared_params.map(|params| params.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(params) = params {
            paths.extend(super::string_values(&request.arguments, &params));
        } else if super::super::current_tool_annotations(&request.tool_name).is_none() {
            paths.extend(super::string_values(
                &request.arguments,
                &[
                    "path",
                    "file",
                    "target",
                    "source_path",
                    "new_path",
                    "target_path",
                    "paths",
                ],
            ));
        }
        paths.extend(
            super::super::super::command_policy::credential_read_path_candidates(
                &request.arguments,
            ),
        );
        paths.sort();
        paths.dedup();
        paths
            .iter()
            .map(|path| {
                crate::workspace_path::classify_permission_path(
                    path,
                    Some(Path::new(&boundary.root)),
                )
            })
            .collect::<Vec<_>>()
    });
    if let Some(annotations) = &request.tool_annotations {
        set_owned_annotations(&mut context, annotations);
    }
    if let Some(paths) = &owned_paths {
        set_owned_paths(&mut context, paths);
        context.literal_identity = Some(super::LiteralResourceIdentity::capture(
            &context,
            &request.arguments,
            declared_params,
        ));
    }
    // Capture only current arguments and host-owned facts. Historical receipt
    // aliases absorbed below cannot enlarge a remembered invocation.
    context.invocation_sha256 = request.workspace_boundary.as_ref().and_then(|boundary| {
        super::invocation_memory::digest(&context, &request.arguments, Path::new(&boundary.root))
    });
    context.absorb_host_value(&request.arguments);
    let policy_context = request
        .policy_decision
        .as_ref()
        .and_then(|decision| decision.get("context"))
        .or_else(|| {
            request
                .approval_request
                .as_ref()
                .and_then(|approval| approval.get("undo_metadata"))
                .and_then(|metadata| metadata.get("policy_decision"))
                .and_then(|decision| decision.get("context"))
        });
    let nested_policy_context = policy_context.and_then(|context| context.get("policy_context"));
    if let Some(policy_context) = policy_context {
        context.tool_kind =
            first_string(policy_context, &["tool_kind", "toolKind"]).or(context.tool_kind);
        context.side_effect = first_string(
            policy_context,
            &[
                "side_effect",
                "sideEffect",
                "requested_side_effect_level",
                "requestedSideEffectLevel",
            ],
        )
        .or(context.side_effect);
        context.agent = first_string(policy_context, &["agent", "agent_id"]).or(context.agent);
        context.persona =
            first_string(policy_context, &["persona", "persona_id"]).or(context.persona);
        context.mode = first_string(policy_context, &["mode", "action"]).or(context.mode);
        context.absorb_host_value(policy_context);
    }
    if let Some(nested_policy_context) = nested_policy_context {
        if context.side_effect.is_none() {
            context.side_effect = first_string(
                nested_policy_context,
                &[
                    "side_effect",
                    "sideEffect",
                    "requested_side_effect_level",
                    "requestedSideEffectLevel",
                ],
            );
        }
        context.absorb_host_value(nested_policy_context);
    }
    for container in [policy_context, Some(&request.arguments)]
        .into_iter()
        .flatten()
    {
        for key in ["rawInput", "raw_input", "input"] {
            if let Some(input) = container.get(key) {
                context.absorb_host_value(input);
            }
        }
    }
    context.finish_host_normalization();
    // Explicit workspace facts own the boundary. Historical receipt aliases
    // cannot replace them for guards or for a captured literal grant.
    if let Some(paths) = &owned_paths {
        set_owned_paths(&mut context, paths);
    }
    if let Some(annotations) = &request.tool_annotations {
        set_owned_annotations(&mut context, annotations);
        // The literal identity is captured before historical receipts are absorbed.
        // Its path declarations must come from this same catalog entry.
    }
    context
}

fn set_owned_annotations(
    context: &mut EvaluationContext,
    annotations: &crate::tool_annotations::ToolAnnotations,
) {
    context.tool_kind = Some(super::tool_kind_string(annotations.kind).to_string());
    context.side_effect = Some(annotations.side_effect_level.as_str().to_string());
    context.capabilities = annotations
        .capabilities
        .iter()
        .flat_map(|(capability, operations)| {
            operations
                .iter()
                .map(move |operation| format!("{capability}.{operation}"))
        })
        .collect();
}

fn set_owned_paths(
    context: &mut EvaluationContext,
    paths: &[crate::workspace_path::WorkspacePathInfo],
) {
    context.path_entries = paths.to_vec();
    context.path_candidates = paths
        .iter()
        .flat_map(|path| path.policy_candidates())
        .collect();
    super::dedup(&mut context.path_candidates);
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
    let modes = if rule.identity_match == PolicyIdentityMatch::Literal {
        let Some(original) = &ctx.literal_identity else {
            return false;
        };
        &original.constraints.env_mode
    } else {
        &ctx.env_modes
    };
    normalized_env_modes(modes)
        .iter()
        .filter(|mode| matches!(mode.as_str(), "patch" | "replace"))
        .all(|mode| rule.matches.env_mode.contains(mode))
}

#[cfg(test)]
mod decoder_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_policy_rejects_matchers_that_shorthand_would_discard() {
        for alias in ["match", "matches", "when"] {
            for action in ["allow", "ask", "deny", "require_approval"] {
                for field in ["command", "commmand"] {
                    let rule = json!({"source": "user", (action): "run",
                        (alias): {(field): "git push"}});
                    let error =
                        ToolApprovalPolicy::from_host_json(json!({"rules": [rule]})).unwrap_err();
                    assert!(
                        error.contains("nested matcher"),
                        "{alias}/{action}/{field}: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn host_policy_rejects_unknown_nested_matchers_without_widening_rules() {
        for key in [
            "allow",
            "ask",
            "deny",
            "require_approval",
            "match",
            "matches",
            "when",
        ] {
            let mut rule = json!({(key): {"tool": "run", "commmand": "git push"}});
            if ["match", "matches", "when"].contains(&key) {
                rule["action"] = json!("allow");
            }
            let error = ToolApprovalPolicy::from_host_json(json!({"rules": [rule]})).unwrap_err();
            assert!(error.contains("commmand"), "{key}: {error}");
        }
        assert!(ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"action": "allow", "tool": "run", "commmand": "git push"}
        ]}))
        .is_err());
        assert!(ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"allow": "run", "approval": {"grant_option": ["once"]}}
        ]}))
        .is_err());
        let valid = ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"source": "mode", "ask": "run"},
            {"source": "user", "allow": {"tools": "run", "commands": "git push"},
             "approval": {"grant_options": ["once"], "metadata": {"custom": "retained"}}}
        ]}))
        .unwrap();
        let request = ToolApprovalRequest::from_host_json(json!({
            "tool_name": "run", "arguments": {"command": "printf unexpected"}
        }))
        .unwrap();
        assert!(valid.evaluate_request(&request).is_ask());
        assert_eq!(
            valid.rules[1].approval.metadata,
            Some(json!({"custom": "retained"}))
        );
    }
}
