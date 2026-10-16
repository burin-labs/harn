//! Combine independent path memories without widening their other conditions.

use super::{
    host_request, ApprovalShape, Candidate, EvaluationContext, PolicyAction, PolicyIdentityMatch,
    PolicyMatchedRule, PolicyRuleSource, ToolApprovalPolicy,
};

pub(super) fn candidates(
    policy: &ToolApprovalPolicy,
    context: &EvaluationContext,
) -> Vec<Candidate> {
    let Some(identity) = &context.literal_identity else {
        return Vec::new();
    };
    if identity.paths.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<(ApprovalShape, Vec<bool>, Vec<PolicyMatchedRule>)> = Vec::new();
    for (index, rule) in policy.rules.iter().enumerate() {
        if rule.source != PolicyRuleSource::User
            || rule.action != PolicyAction::Allow
            || rule.identity_match != PolicyIdentityMatch::Literal
            || rule.matches.path.is_empty()
            || !rule.matches.url.is_empty()
            || !rule.matches.domain.is_empty()
            || !host_request::exact_write_env_allow(rule, context)
        {
            continue;
        }
        let mut other_conditions = rule.matches.clone();
        other_conditions.path.clear();
        if !other_conditions.matches(context, rule.identity_match, rule.action) {
            continue;
        }
        let covered = identity
            .paths
            .iter()
            .map(|aliases| aliases.iter().any(|path| rule.matches.path.contains(path)))
            .collect::<Vec<_>>();
        if !covered.iter().any(|covered| *covered) {
            continue;
        }
        let group_index = groups
            .iter()
            .position(|(approval, _, _)| approval == &rule.approval)
            .unwrap_or_else(|| {
                groups.push((
                    rule.approval.clone(),
                    vec![false; identity.paths.len()],
                    Vec::new(),
                ));
                groups.len() - 1
            });
        let (_, coverage, contributors) = &mut groups[group_index];
        for (total, covered) in coverage.iter_mut().zip(covered) {
            *total |= covered;
        }
        contributors.push(PolicyMatchedRule {
            source: rule.source.receipt_source().into(),
            action: rule.action.as_str().into(),
            id: rule.id.clone(),
            index: Some(index),
            contributing_rules: Vec::new(),
        });
    }
    groups
        .into_iter()
        .filter(|(_, coverage, contributors)| {
            contributors.len() > 1 && coverage.iter().all(|covered| *covered)
        })
        .map(|(approval, _, contributing_rules)| Candidate {
            source: PolicyRuleSource::User.receipt_source().into(),
            source_rank: PolicyRuleSource::User,
            index: None,
            id: None,
            action: PolicyAction::Allow,
            reason: format!(
                "{} remembered path grants cover every requested resource",
                contributing_rules.len()
            ),
            approval,
            risk_labels: vec!["path_rule".into()],
            denied_paths: Vec::new(),
            contributing_rules,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ToolApprovalRequest;
    use serde_json::json;

    #[test]
    fn independent_memories_cover_every_path_without_expanding_authority() {
        let workspace = tempfile::tempdir().unwrap();
        let request = ToolApprovalRequest::from_host_json(json!({
            "tool_name": "read", "arguments": {"paths": ["first.txt", "second.txt"]},
            "workspace_boundary": {"root": workspace.path().to_str().unwrap()},
            "tool_annotations": {"kind": "read", "arg_schema": {"path_params": ["paths"]}}
        }))
        .unwrap();
        let rules = json!([
            {"id": "ask", "source": "mode", "ask": {"tool": "read"}},
            {"id": "first", "source": "user", "identity_match": "literal", "allow": {"tool": "read", "path": "first.txt"}},
            {"id": "second", "source": "user", "identity_match": "literal", "allow": {"tool": "read", "path": "second.txt"}}
        ]);
        let evaluate = |rules: serde_json::Value, request: &ToolApprovalRequest| {
            ToolApprovalPolicy::from_host_json(json!({"rules": rules}))
                .unwrap()
                .evaluate_request(request)
        };
        let allowed = evaluate(rules.clone(), &request);
        assert!(allowed.is_allow(), "{allowed:?}");
        let matched = allowed.matched_rule.as_ref().unwrap();
        assert_eq!(matched.source, "user");
        assert!(matched.index.is_none() && matched.id.is_none());
        assert_eq!(
            matched
                .contributing_rules
                .iter()
                .map(|rule| (rule.id.as_deref(), rule.index))
                .collect::<Vec<_>>(),
            vec![(Some("first"), Some(1)), (Some("second"), Some(2))]
        );
        assert_eq!(
            allowed.receipt["matched_rule"]["contributing_rules"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        for paths in [json!(["first.txt", "second.txt", "extra.txt"]), json!([])] {
            let mut changed = request.clone();
            changed.arguments["paths"] = paths;
            let refused = evaluate(rules.clone(), &changed);
            assert!(refused.is_ask(), "{refused:?}");
        }
        let mut wrong_tool = rules.clone();
        wrong_tool[2]["allow"]["tool"] = json!("write");
        assert!(evaluate(wrong_tool, &request).is_ask());
        let mut different_source = rules.clone();
        different_source[2]["source"] = json!("mode");
        assert!(evaluate(different_source, &request).is_ask());
        let mut different_approval = rules.clone();
        different_approval[2]["approval"] = json!({"risk": "other authority"});
        assert!(evaluate(different_approval, &request).is_ask());
        let mut cross_resource = rules.clone();
        cross_resource[2]["allow"]["url"] = json!("https://example.org");
        assert!(evaluate(cross_resource, &request).is_ask());
        let mut environment = request.clone();
        environment.arguments["env_mode"] = json!("patch");
        assert!(evaluate(rules.clone(), &environment).is_ask());
        let mut exact_environment = rules.clone();
        exact_environment[1]["allow"]["env_mode"] = json!("patch");
        exact_environment[2]["allow"]["env_mode"] = json!("patch");
        assert!(evaluate(exact_environment, &environment).is_allow());
        let malformed = ToolApprovalRequest {
            tool_name: "read".into(),
            arguments: json!({"paths": ["first.txt", "second.txt", 123]}),
            ..Default::default()
        };
        let malformed_decision = evaluate(rules.clone(), &malformed);
        assert!(malformed_decision.is_deny());
        assert_eq!(
            malformed_decision.receipt["matched_rule"]["id"],
            "invalid_host_request"
        );
        let mut constrained = rules.clone();
        constrained
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "supervised", "source": "policy", "ask": {"tool": "read"}}));
        let asked = evaluate(constrained, &request);
        assert!(asked.is_ask());
        assert_eq!(asked.receipt["matched_rule"]["id"], "supervised");
        let mut denied = rules;
        denied.as_array_mut().unwrap().push(json!({"id": "deny-second", "source": "user", "identity_match": "literal", "deny": {"tool": "read", "path": "second.txt"}}));
        let refusal = evaluate(denied, &request);
        assert!(refusal.is_deny());
        assert_eq!(refusal.receipt["matched_rule"]["id"], "deny-second");
    }
}
