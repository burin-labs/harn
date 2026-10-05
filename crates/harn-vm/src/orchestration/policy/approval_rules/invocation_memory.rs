//! Exact remembered decisions shared by native, terminal and VM hosts.
//!
//! Persist an opaque fingerprint, not arguments that may contain credentials.
//! All arguments, canonical workspace and owned tool facts contribute; display
//! receipts, alias projections and repetition counters do not supply authority.

use std::path::Path;

use serde_json::{json, Value as JsonValue};

use super::{
    ApprovalShape, EvaluationContext, PolicyAction, PolicyIdentityMatch, PolicyRule,
    PolicyRuleMatch, PolicyRuleSource, ToolApprovalRequest,
};

pub(super) fn digest(
    context: &EvaluationContext,
    arguments: &JsonValue,
    workspace: &Path,
) -> Option<String> {
    let root = workspace.canonicalize().ok()?;
    if !root.is_dir() || !arguments.is_object() {
        return None;
    }
    // Hosts and VM dispatch may enumerate the same classified paths in
    // different orders. The invocation already binds argument-array order;
    // this derived classification census has one canonical ordering here.
    let mut paths = context
        .path_entries
        .iter()
        .map(|path| crate::canonical_json::to_vec(&json!(path)))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Some(super::stable_json_digest(&json!({
        "schema": "harn.remembered_invocation.v1",
        "workspace": root,
        "tool": context.tool_name,
        "arguments": arguments,
        "constraints": context.invocation_constraints(),
        "paths": paths,
    })))
}

impl ToolApprovalRequest {
    /// Capture an explicit user decision for this whole invocation.
    ///
    /// Both session memory and durable policy use the returned rule unchanged.
    /// The host owns storage, IDs and consent presentation; Harn owns scope.
    pub fn capture_decision(&self, action: PolicyAction) -> Result<PolicyRule, String> {
        self.validate()?;
        if !matches!(action, PolicyAction::Allow | PolicyAction::Deny) {
            return Err("remembered decisions must be allow or deny".into());
        }
        if self.workspace_boundary.is_none() {
            return Err("remembered decisions require an explicit workspace_boundary".into());
        }
        let context = super::host_request::request_context(self);
        let env_mode = context
            .literal_identity
            .as_ref()
            .ok_or("remembered decision has no original invocation constraints")?
            .constraints
            .env_mode
            .clone();
        let invocation_sha256 = context
            .invocation_sha256
            .ok_or("remembered decision has no canonical invocation identity")?;
        Ok(PolicyRule {
            id: None,
            action,
            source: PolicyRuleSource::User,
            identity_match: PolicyIdentityMatch::Literal,
            matches: PolicyRuleMatch {
                tool: vec![self.tool_name.clone()],
                invocation_sha256: Some(invocation_sha256),
                env_mode,
                ..Default::default()
            },
            reason: None,
            approval: ApprovalShape::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::policy::ToolApprovalPolicy;

    fn request(root: &Path, arguments: JsonValue) -> ToolApprovalRequest {
        ToolApprovalRequest::from_host_json(json!({
            "tool_name": "run", "arguments": arguments,
            "workspace_boundary": {"root": root}
        }))
        .unwrap()
    }

    fn policy(rule: PolicyRule) -> ToolApprovalPolicy {
        ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"source": "mode", "ask": "run"}, rule
        ]}))
        .unwrap()
    }

    #[test]
    fn remembered_decision_binds_all_arguments_and_workspace_without_persisting_them() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let original = request(
            first.path(),
            json!({
                "command": "echo harmless", "paths": ["first", "second"],
                "env": {"TOKEN": "secret-sentinel"}, "argv": ["a b", "c"]
            }),
        );
        let rule = original.capture_decision(PolicyAction::Allow).unwrap();
        let serialized = serde_json::to_string(&rule).unwrap();
        assert!(!serialized.contains("secret-sentinel"));
        assert!(!serialized.contains("echo harmless"));
        let restored: PolicyRule = serde_json::from_str(&serialized).unwrap();
        let saved = policy(restored);
        assert!(saved.evaluate_request(&original).is_allow());
        for replacement in [
            json!({"command": "echo changed"}),
            json!({"paths": ["first", "changed"]}),
            json!({"env": {"TOKEN": "changed"}}),
            json!({"argv": ["a", "b c"]}),
        ] {
            let mut changed = original.clone();
            for (key, value) in replacement.as_object().unwrap() {
                changed.arguments[key] = value.clone();
            }
            // Old receipts cannot turn the changed invocation back into a grant.
            changed.policy_decision = Some(json!({"context": {"rawInput": original.arguments}}));
            assert!(saved.evaluate_request(&changed).is_ask(), "{replacement}");
        }
        let other_workspace = request(second.path(), original.arguments.clone());
        assert!(saved.evaluate_request(&other_workspace).is_ask());
        let mut no_boundary = original.clone();
        no_boundary.workspace_boundary = None;
        assert!(saved.evaluate_request(&no_boundary).is_ask());
        assert!(no_boundary.capture_decision(PolicyAction::Allow).is_err());
        assert!(original.capture_decision(PolicyAction::Ask).is_err());
        let denied = policy(original.capture_decision(PolicyAction::Deny).unwrap());
        assert!(denied.evaluate_request(&original).is_deny());
        assert!(denied.evaluate_request(&other_workspace).is_ask());
    }

    #[test]
    fn remembered_decision_ignores_receipts_and_json_object_order() {
        let root = tempfile::tempdir().unwrap();
        let original = request(
            root.path(),
            json!({"command": "echo ok", "argv": ["a", "b"]}),
        );
        let mut same = request(
            root.path(),
            json!({"argv": ["a", "b"], "command": "echo ok"}),
        );
        same.repeat_count = Some(99);
        same.policy_decision = Some(json!({"context": {"command": "invented", "agent": "old"}}));
        let rule = original.capture_decision(PolicyAction::Allow).unwrap();
        assert_eq!(
            rule.matches,
            same.capture_decision(PolicyAction::Allow).unwrap().matches
        );
        assert!(policy(rule).evaluate_request(&same).is_allow());
    }

    #[test]
    fn remembered_write_environment_grants_cover_only_the_captured_mode() {
        let root = tempfile::tempdir().unwrap();
        for mode in ["patch", "replace"] {
            let original = request(root.path(), json!({"command": "echo ok", "env_mode": mode}));
            let saved = policy(original.capture_decision(PolicyAction::Allow).unwrap());
            assert!(saved.evaluate_request(&original).is_allow(), "{mode}");
            for stale_mode in ["inherit_clean", "patch", "replace"] {
                let mut stale = original.clone();
                stale.policy_decision = Some(json!({"context": {"env_mode": stale_mode}}));
                assert_eq!(
                    original
                        .capture_decision(PolicyAction::Allow)
                        .unwrap()
                        .matches,
                    stale.capture_decision(PolicyAction::Allow).unwrap().matches
                );
                assert!(saved.evaluate_request(&stale).is_allow());
            }
            let mut changed = original.clone();
            changed.arguments["env_mode"] =
                json!(if mode == "patch" { "replace" } else { "patch" });
            assert!(saved.evaluate_request(&changed).is_ask());
        }
    }

    #[test]
    fn host_capture_matches_vm_dispatch_with_mixed_path_order() {
        use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
        let root = crate::orchestration::execution_root_path()
            .canonicalize()
            .unwrap();
        let mut original = request(&root, json!({"paths": ["b", root.join("z")]}));
        original.tool_annotations = Some(
            serde_json::from_value(json!({
                "kind": "read", "arg_schema": {"path_params": ["paths"]}
            }))
            .unwrap(),
        );
        let saved = policy(original.capture_decision(PolicyAction::Allow).unwrap());
        assert!(saved.evaluate_request(&original).is_allow());
        let mut ambient = CapabilityPolicy::default();
        ambient
            .tool_annotations
            .insert("run".into(), original.tool_annotations.clone().unwrap());
        push_execution_policy(ambient);
        let result =
            super::super::evaluate_tool_approval_policy(&saved, "run", &original.arguments, None);
        pop_execution_policy();
        assert!(result.is_allow(), "{result:?}");
    }
}
