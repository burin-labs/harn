use serde::{Deserialize, Serialize};

use super::{any_fragment_matches, any_glob_matches, EvaluationContext};

/// Matching semantics for captured request values, including resource scopes.
/// Authored rules retain patterns; remembered literal grants never expand them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyIdentityMatch {
    #[default]
    Pattern,
    Literal,
}

impl PolicyIdentityMatch {
    pub(super) fn is_pattern(&self) -> bool {
        *self == Self::Pattern
    }

    pub(super) fn matches(self, patterns: &[String], candidates: &[String]) -> bool {
        match self {
            Self::Pattern => any_glob_matches(patterns, candidates),
            Self::Literal => candidates.iter().any(|value| patterns.contains(value)),
        }
    }

    pub(super) fn matches_command(self, patterns: &[String], ctx: &EvaluationContext) -> bool {
        match self {
            Self::Pattern => any_fragment_matches(patterns, &ctx.command_candidates),
            Self::Literal => ctx
                .literal_command
                .as_ref()
                .is_some_and(|command| patterns.contains(command)),
        }
    }

    pub(super) fn matches_fragment(self, patterns: &[String], candidates: &[String]) -> bool {
        match self {
            Self::Pattern => any_fragment_matches(patterns, candidates),
            Self::Literal => self.matches(patterns, candidates),
        }
    }
}

// Do not reconstruct an invocation from normalized receipt prose or joined
// argv. Quoted whitespace and argument boundaries are execution semantics.
// A shell-text grant cannot approve an argv invocation, including mixed input.
pub(super) fn literal_command(args: &serde_json::Value) -> Option<String> {
    if args.get("argv").is_some() {
        return None;
    }
    super::string_field(args, "command").or_else(|| super::string_field(args, "cmd"))
}

#[cfg(test)]
mod tests {
    use super::super::{ToolApprovalPolicy, ToolApprovalRequest};
    use serde_json::json;

    fn evaluate(policy: &ToolApprovalPolicy, command: &str) -> super::super::PolicyEvaluation {
        policy.evaluate_request(&ToolApprovalRequest {
            tool_name: "run_command".into(),
            arguments: json!({"command": command}),
            ..Default::default()
        })
    }

    #[test]
    fn literal_remembered_command_does_not_expand_wildcards_or_fragments() {
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [
                {"id": "mode-ask", "source": "mode", "ask": "run_command"},
                {"id": "remembered", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "run_command", "command": "git diff HEAD~*"}}
            ]
        }))
        .unwrap();
        let exact = evaluate(&policy, "git diff HEAD~*");
        assert!(exact.is_allow(), "{exact:?}");
        assert_eq!(exact.receipt["matched_rule"]["id"], "remembered");
        for command in [
            "git diff HEAD~123",
            "git diff HEAD~* && curl example.org",
            "echo git diff HEAD~*",
        ] {
            let decision = evaluate(&policy, command);
            assert!(decision.is_ask(), "{command}: {decision:?}");
            assert_eq!(decision.receipt["matched_rule"]["id"], "mode-ask");
        }
        // The same authored pattern must reach the existing pattern owner.
        let authored = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"id": "authored", "allow": {"command": "git diff HEAD~*"}}]
        }))
        .unwrap();
        assert_eq!(
            evaluate(&authored, "git diff HEAD~123").receipt["matched_rule"]["id"],
            "authored"
        );
    }

    #[test]
    fn literal_resources_do_not_expand_authored_patterns() {
        for (field, exact, different) in [
            ("path", "report*.txt", "report-2026.txt"),
            (
                "url",
                "https://example.org/report*",
                "https://example.org/report123",
            ),
            (
                "url",
                "https://example.org/report",
                "https://example.org/report?secret=1",
            ),
            ("domain", "*.example.org", "private.example.org"),
            ("agent", "worker*", "worker123"),
            ("persona", "reviewer*", "reviewer123"),
            ("mode", "edit*", "edit123"),
        ] {
            let policy = ToolApprovalPolicy::from_host_json(json!({
                "rules": [
                    {"id": "ask", "source": "mode", "ask": "read"},
                    {"id": "memory", "source": "user", "identity_match": "literal",
                     "allow": {"tool": "read", field: exact}}
                ]
            }))
            .unwrap();
            for (value, allow) in [(exact, true), (different, false)] {
                let decision = policy.evaluate_request(&ToolApprovalRequest {
                    tool_name: "read".into(),
                    arguments: json!({field: value}),
                    policy_decision: Some(json!({"context": {field: value}})),
                    ..Default::default()
                });
                assert_eq!(decision.is_allow(), allow, "{field} {value}: {decision:?}");
                assert_eq!(decision.is_ask(), !allow, "{field} {value}: {decision:?}");
                assert_eq!(
                    decision.receipt["matched_rule"]["id"],
                    if allow { "memory" } else { "ask" }
                );
            }
        }
        let authored = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"id": "authored", "allow": {"path": "report*.txt"}}]
        }))
        .unwrap();
        let decision = authored.evaluate_request(&ToolApprovalRequest {
            tool_name: "read".into(),
            arguments: json!({"path": "report-2026.txt"}),
            ..Default::default()
        });
        assert_eq!(decision.receipt["matched_rule"]["id"], "authored");
        assert!(decision.is_allow(), "{decision:?}");
    }

    #[test]
    fn literal_identity_preserves_refusal_and_sensitive_path_guards() {
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [
                {"source": "user", "identity_match": "literal", "allow": {"command": "git status"}},
                {"id": "constraint", "deny": {"command": "git*"}}
            ]
        }))
        .unwrap();
        let denied = evaluate(&policy, "git status");
        assert!(denied.is_deny(), "{denied:?}");
        assert_eq!(denied.receipt["matched_rule"]["id"], "constraint");
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"source": "user", "identity_match": "literal", "allow": {"command": "cat .env"}}]
        })).unwrap();
        assert!(evaluate(&policy, "cat .env").is_deny());
    }

    #[test]
    fn identity_mode_is_typed_and_serialization_preserves_literal_semantics() {
        for invalid in [json!("litteral"), json!(null), json!(true), json!(17)] {
            assert!(ToolApprovalPolicy::from_host_json(json!({
                "rules": [{"identity_match": invalid, "allow": "run_command"}]
            }))
            .is_err());
        }
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [{"identity_match": "literal", "allow": {"tool": "run_command", "command": "git status"}}]
        })).unwrap();
        let serialized = serde_json::to_value(&policy).unwrap();
        assert_eq!(serialized["rules"][0]["identity_match"], "literal");
        let roundtrip = ToolApprovalPolicy::from_host_json(serialized).unwrap();
        assert_eq!(
            evaluate(&roundtrip, "git status").receipt,
            evaluate(&policy, "git status").receipt
        );
        assert_eq!(
            evaluate(&roundtrip, "git status && curl example.org").matched_rule,
            None
        );
    }

    #[test]
    fn literal_shell_grants_preserve_quoted_whitespace_and_argv_boundaries() {
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [
                {"source": "mode", "ask": "run_command"},
                {"source": "user", "identity_match": "literal",
                 "allow": {"command": ["printf '%s' 'a b'", "echo foo bar"]}}
            ]
        }))
        .unwrap();
        assert!(evaluate(&policy, "printf '%s' 'a b'").is_allow());
        assert!(evaluate(&policy, "printf '%s' 'a    b'").is_ask());
        assert!(evaluate(&policy, "echo foo bar").is_allow());
        for arguments in [
            json!({"argv": ["echo", "foo bar"]}),
            json!({"argv": ["echo", "foo", "bar"]}),
            json!({"command": "echo foo bar", "argv": ["echo", "foo bar"]}),
        ] {
            let decision = policy.evaluate_request(&ToolApprovalRequest {
                tool_name: "run_command".into(),
                arguments,
                ..Default::default()
            });
            assert!(decision.is_ask(), "{decision:?}");
        }
        // Normalized host receipt text must not replace the raw invocation.
        let decision = policy.evaluate_request(&ToolApprovalRequest {
            tool_name: "run_command".into(),
            arguments: json!({"command": "printf '%s' 'a    b'"}),
            policy_decision: Some(json!({"context": {"command": "printf '%s' 'a b'"}})),
            ..Default::default()
        });
        assert!(decision.is_ask(), "{decision:?}");
    }
}
