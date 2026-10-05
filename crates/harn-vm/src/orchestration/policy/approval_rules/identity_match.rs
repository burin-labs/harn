use serde::{Deserialize, Serialize};

use super::{
    any_fragment_matches, any_glob_matches, EvaluationContext, PolicyAction, PolicyRuleMatch,
};

pub(super) fn resources_match(
    rule: &PolicyRuleMatch,
    context: &EvaluationContext,
    identity: PolicyIdentityMatch,
    action: PolicyAction,
) -> bool {
    match identity {
        PolicyIdentityMatch::Literal => context.literal_identity.as_ref().is_some_and(|raw| {
            raw.paths_match(&rule.path, action)
                && LiteralResourceIdentity::values_match(
                    &rule.url,
                    &raw.urls,
                    raw.urls_valid,
                    action,
                )
                && LiteralResourceIdentity::values_match(
                    &rule.domain,
                    &raw.domains,
                    raw.domains_valid,
                    action,
                )
        }),
        PolicyIdentityMatch::Pattern => {
            (rule.path.is_empty() || identity.matches(&rule.path, &context.path_candidates))
                && (rule.url.is_empty() || identity.matches_fragment(&rule.url, &context.urls))
                && (rule.domain.is_empty() || identity.matches(&rule.domain, &context.domains))
        }
    }
}

pub(super) fn invocation_match(
    rule: &PolicyRuleMatch,
    context: &EvaluationContext,
    identity: PolicyIdentityMatch,
    action: PolicyAction,
) -> bool {
    let normalized;
    let actual = if identity == PolicyIdentityMatch::Literal {
        let Some(raw) = &context.literal_identity else {
            return false;
        };
        &raw.constraints
    } else {
        normalized = context.invocation_constraints();
        &normalized
    };
    let methods = super::normalize_patterns_upper(&rule.http_method);
    [
        (&rule.tool_kind, &actual.tool_kind),
        (&rule.side_effect, &actual.side_effect),
        (&rule.command_identity, &actual.command_identity),
        (&methods, &actual.http_method),
        (&rule.mcp_server, &actual.mcp_server),
        (&rule.mcp_tool, &actual.mcp_tool),
        (&rule.agent, &actual.agent),
        (&rule.persona, &actual.persona),
        (&rule.mode, &actual.mode),
        (&rule.capability, &actual.capability),
    ]
    .into_iter()
    .all(|(patterns, values)| {
        if identity == PolicyIdentityMatch::Literal {
            LiteralResourceIdentity::values_match(patterns, values, true, action)
        } else {
            patterns.is_empty() || identity.matches(patterns, values)
        }
    }) && super::host_request::env_modes_match(&rule.env_mode, &actual.env_mode, identity)
}

#[derive(Clone, Debug)]
pub(super) struct LiteralResourceIdentity {
    pub(super) paths: Vec<Vec<String>>,
    pub(super) urls: Vec<String>,
    pub(super) domains: Vec<String>,
    pub(super) constraints: PolicyRuleMatch,
    urls_valid: bool,
    domains_valid: bool,
}

impl LiteralResourceIdentity {
    pub(super) fn capture(
        context: &EvaluationContext,
        arguments: &serde_json::Value,
        owned_path_params: Option<&[String]>,
    ) -> Self {
        let mut paths = context
            .path_entries
            .iter()
            .map(|entry| entry.policy_candidates())
            .collect::<Vec<_>>();
        for path in &context.path_candidates {
            if !paths.iter().any(|aliases| aliases.contains(path)) {
                paths.push(vec![path.clone()]);
            }
        }
        // Raw argument objects cannot supply trusted path classifications.
        // Keep an uncovered group rather than dropping malformed resources.
        let annotations = super::super::current_tool_annotations(&context.tool_name);
        let path_keys = owned_path_params
            .map(|params| params.iter().map(String::as_str).collect())
            .unwrap_or_else(|| {
                annotations.as_ref().map_or_else(
                    || super::path_inputs::CONVENTIONAL_PATH_PARAMETERS.to_vec(),
                    |annotations| {
                        annotations
                            .arg_schema
                            .path_params
                            .iter()
                            .map(String::as_str)
                            .collect()
                    },
                )
            });
        if !resource_values_valid(arguments, &path_keys) {
            paths.push(Vec::new());
        }
        let mut strings = Vec::new();
        super::collect_string_values(arguments, &mut strings);
        // Literal identities retain the invocation's spelling. URL parser
        // normalization is for authored patterns, not an extra resource.
        let mut urls = strings
            .into_iter()
            .filter(|value| {
                url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
            })
            .collect::<Vec<_>>();
        urls.extend(super::string_values(arguments, &["url", "urls"]));
        let urls_valid = resource_values_valid(arguments, &["url", "urls"]);
        let mut domains = context.domains.clone();
        domains.extend(super::string_values(arguments, &["domain", "domains"]));
        let domains_valid = resource_values_valid(arguments, &["domain", "domains"]);
        Self {
            paths,
            urls,
            domains,
            constraints: context.invocation_constraints(),
            urls_valid,
            domains_valid,
        }
    }

    pub(super) fn paths_match(&self, values: &[String], action: PolicyAction) -> bool {
        if values.is_empty() {
            return true;
        }
        let covered = |aliases: &Vec<String>| aliases.iter().any(|value| values.contains(value));
        !self.paths.is_empty()
            && if action == PolicyAction::Allow {
                self.paths.iter().all(covered)
            } else {
                self.paths.iter().any(covered)
            }
    }

    pub(super) fn values_match(
        values: &[String],
        resources: &[String],
        valid: bool,
        action: PolicyAction,
    ) -> bool {
        if values.is_empty() {
            return true;
        }
        !resources.is_empty()
            && if action == PolicyAction::Allow {
                valid && resources.iter().all(|resource| values.contains(resource))
            } else {
                resources.iter().any(|resource| values.contains(resource))
            }
    }
}

fn resource_values_valid(arguments: &serde_json::Value, keys: &[&str]) -> bool {
    let valid_string =
        |value: &serde_json::Value| value.as_str().is_some_and(|value| !value.trim().is_empty());
    keys.iter().all(|key| match arguments.get(*key) {
        None => true,
        Some(serde_json::Value::Array(values)) => values.iter().all(valid_string),
        Some(value) => valid_string(value),
    })
}

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
    use super::super::{ToolApprovalPolicy, ToolApprovalRequest, ToolApprovalWorkspaceBoundary};
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
        let workspace = tempfile::tempdir().unwrap();
        let boundary = ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_str().unwrap().into(),
        };
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
                    workspace_boundary: Some(boundary.clone()),
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
            workspace_boundary: Some(boundary),
            ..Default::default()
        });
        assert_eq!(decision.receipt["matched_rule"]["id"], "authored");
        assert!(decision.is_allow(), "{decision:?}");
    }

    #[test]
    fn literal_allow_covers_every_resource_and_deny_covers_any() {
        let workspace = tempfile::tempdir().unwrap();
        let boundary = ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_str().unwrap().into(),
        };
        for (field, argument, saved, extra) in [
            ("path", "paths", "saved.txt", "extra.txt"),
            (
                "url",
                "urls",
                "https://saved.example/report",
                "https://extra.example/report",
            ),
            ("domain", "domains", "saved.example", "extra.example"),
        ] {
            let allow = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "ask", "source": "mode", "ask": "read"},
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "read", field: [saved, extra]}},
            ]}))
            .unwrap();
            for values in [json!([saved]), json!([saved, extra])] {
                let decision = super::super::evaluate_tool_approval_policy(
                    &allow,
                    "read",
                    &json!({argument: values}),
                    None,
                );
                assert!(decision.is_allow(), "{field}: {decision:?}");
                assert_eq!(decision.receipt["matched_rule"]["id"], "memory");
            }
            let partial = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "ask", "source": "mode", "ask": "read"},
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "read", field: saved}},
            ]}))
            .unwrap();
            for values in [json!([saved, extra]), json!([])] {
                let decision = partial.evaluate_request(&ToolApprovalRequest {
                    tool_name: "read".into(),
                    arguments: json!({argument: values}),
                    workspace_boundary: Some(boundary.clone()),
                    ..Default::default()
                });
                assert!(decision.is_ask(), "{field}: {decision:?}");
                assert_eq!(decision.receipt["matched_rule"]["id"], "ask");
            }
            let deny = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "deny": {"tool": "read", field: saved}},
            ]}))
            .unwrap();
            let decision = deny.evaluate_request(&ToolApprovalRequest {
                tool_name: "read".into(),
                arguments: json!({argument: [saved, extra]}),
                workspace_boundary: Some(boundary.clone()),
                ..Default::default()
            });
            assert!(decision.is_deny(), "{field}: {decision:?}");
            assert_eq!(decision.receipt["matched_rule"]["id"], "memory");
        }
    }

    #[test]
    fn caller_path_descriptors_cannot_supply_remembered_aliases() {
        let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"id": "ask", "source": "mode", "ask": "read"},
            {"id": "memory", "source": "user", "identity_match": "literal",
             "allow": {"tool": "read", "path": "saved.txt"}},
        ]}))
        .unwrap();
        let decision = policy.evaluate_request(&ToolApprovalRequest {
            tool_name: "read".into(),
            arguments: json!({"paths": [{
                "input": "outside.txt", "workspace_path": "saved.txt",
                "path": "outside.txt", "host_path": "/outside.txt"
            }]}),
            ..Default::default()
        });
        assert!(decision.is_deny(), "{decision:?}");
        assert_eq!(
            decision.receipt["matched_rule"]["id"],
            "invalid_host_request"
        );
    }

    #[test]
    fn malformed_resource_is_not_a_matchable_empty_identity() {
        for (field, argument, saved) in [
            ("url", "urls", "https://saved.example/report"),
            ("domain", "domains", "saved.example"),
        ] {
            let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "ask", "source": "mode", "ask": "read"},
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "read", field: [saved, ""]}},
            ]}))
            .unwrap();
            let decision = policy.evaluate_request(&ToolApprovalRequest {
                tool_name: "read".into(),
                arguments: json!({argument: [saved, 123]}),
                ..Default::default()
            });
            assert!(decision.is_ask(), "{field}: {decision:?}");
            assert_eq!(decision.receipt["matched_rule"]["id"], "ask");
        }
    }

    #[test]
    fn literal_url_keeps_the_raw_invocation_spelling() {
        for saved in ["https://example.org", "https://EXAMPLE.org:443/report"] {
            let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "ask", "source": "mode", "ask": "read"},
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "read", "url": saved}},
            ]}))
            .unwrap();
            let decision = policy.evaluate_request(&ToolApprovalRequest {
                tool_name: "read".into(),
                arguments: json!({"url": saved}),
                ..Default::default()
            });
            assert!(decision.is_allow(), "{saved}: {decision:?}");
            assert_eq!(decision.receipt["matched_rule"]["id"], "memory");
        }
    }

    #[test]
    fn remembered_resource_cannot_grant_a_different_tool_named_by_receipt() {
        let workspace = tempfile::tempdir().unwrap();
        let boundary = ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_str().unwrap().into(),
        };
        let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"id": "ask", "source": "mode", "ask": "*"},
            {"id": "memory", "source": "user", "identity_match": "literal",
             "allow": {"tool": "read", "path": "saved.txt"}},
        ]}))
        .unwrap();
        for (tool, allow) in [("read", true), ("write", false)] {
            let decision = policy.evaluate_request(&ToolApprovalRequest {
                tool_name: tool.into(),
                arguments: json!({"path": "saved.txt"}),
                policy_decision: Some(json!({"context": {"tool_name": "read"}})),
                workspace_boundary: Some(boundary.clone()),
                ..Default::default()
            });
            assert_eq!(decision.is_allow(), allow, "{tool}: {decision:?}");
            assert_eq!(decision.is_ask(), !allow, "{tool}: {decision:?}");
            assert_eq!(
                decision.receipt["matched_rule"]["id"],
                if allow { "memory" } else { "ask" }
            );
        }
    }

    #[test]
    fn remembered_resources_cannot_match_stale_host_receipts() {
        let workspace = tempfile::tempdir().unwrap();
        let boundary = ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_str().unwrap().into(),
        };
        for (field, saved, different) in [
            ("path", "saved.txt", "different.txt"),
            (
                "url",
                "https://saved.example/report",
                "https://different.example/report",
            ),
            ("domain", "saved.example", "different.example"),
        ] {
            let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"id": "ask", "source": "mode", "ask": "read"},
                {"id": "memory", "source": "user", "identity_match": "literal",
                 "allow": {"tool": "read", field: saved}},
            ]}))
            .unwrap();
            let exact = policy.evaluate_request(&ToolApprovalRequest {
                tool_name: "read".into(),
                arguments: json!({field: saved}),
                workspace_boundary: Some(boundary.clone()),
                ..Default::default()
            });
            assert!(exact.is_allow(), "{field}: {exact:?}");
            assert_eq!(exact.receipt["matched_rule"]["id"], "memory");
            for context in [
                json!({field: saved}),
                json!({"rawInput": {field: saved}}),
                json!({"policy_context": {field: saved}}),
            ] {
                let request = ToolApprovalRequest {
                    tool_name: "read".into(),
                    arguments: json!({field: different}),
                    policy_decision: Some(json!({"context": context})),
                    workspace_boundary: Some(boundary.clone()),
                    ..Default::default()
                };
                let refused = policy.evaluate_request(&request);
                assert!(refused.is_ask(), "{field}: {refused:?}");
                assert_eq!(refused.receipt["matched_rule"]["id"], "ask");
                // Authored policy still consumes host facts. This correction
                // only restricts which invocation a captured grant can name.
                let authored = ToolApprovalPolicy::from_host_json(json!({"rules": [
                    {"id": "authored", "allow": {field: saved}},
                ]}))
                .unwrap();
                let authored_decision = authored.evaluate_request(&request);
                if field == "path" {
                    // Workspace-classified raw paths own authorization; a
                    // historical receipt cannot introduce another path.
                    assert!(authored_decision.is_allow());
                    assert!(authored_decision.matched_rule.is_none());
                } else {
                    assert_eq!(authored_decision.receipt["matched_rule"]["id"], "authored");
                }
            }
        }
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

    #[test]
    fn literal_http_grants_ignore_stale_receipt_methods() {
        let policy = ToolApprovalPolicy::from_host_json(json!({
            "rules": [
                {"source": "mode", "ask": "fetch"},
                {"source": "user", "identity_match": "literal",
                 "allow": {"tool": "fetch", "url": "https://example.org/report", "method": "GET"}}
            ]
        }))
        .unwrap();
        for (method, stale, expected_allow) in [
            ("GET", false, true),
            ("POST", false, false),
            ("POST", true, false),
        ] {
            let request = ToolApprovalRequest::from_host_json(json!({
                "tool_name": "fetch",
                "arguments": {"url": "https://example.org/report", "method": method},
                "policy_decision": {"context": {"http_methods": if stale { vec!["GET"] } else { vec![] }}}
            })).unwrap();
            let decision = policy.evaluate_request(&request);
            assert_eq!(decision.is_allow(), expected_allow, "{decision:?}");
            assert_eq!(decision.is_ask(), !expected_allow, "{decision:?}");
        }
    }

    #[test]
    fn literal_invocation_constraints_do_not_absorb_receipt_aliases() {
        for (matcher, argument, receipt_key, original, changed) in [
            ("method", "method", "http_methods", "GET", "POST"),
            ("mcp_server", "mcp_server", "mcp_servers", "saved", "other"),
            ("mcp_tool", "mcp_tool", "mcp_tools", "read", "write"),
            ("env_mode", "env_mode", "env_modes", "inherit_clean", "none"),
            ("agent", "agent", "agent", "saved", "other"),
            ("persona", "persona", "persona", "saved", "other"),
            ("mode", "mode", "mode", "saved", "other"),
        ] {
            let policy = ToolApprovalPolicy::from_host_json(json!({
                "rules": [
                    {"source": "mode", "ask": "invoke"},
                    {"source": "user", "identity_match": "literal", "allow": {"tool": "invoke", (matcher): original}}
                ]
            })).unwrap();
            for (value, expected_allow) in [(original, true), (changed, false)] {
                let request = ToolApprovalRequest::from_host_json(json!({
                    "tool_name": "invoke", "arguments": {(argument): value},
                    "policy_decision": {"context": {(receipt_key): if ["agent", "persona", "mode"].contains(&receipt_key) { json!(original) } else { json!([original]) }}}
                }))
                .unwrap();
                let decision = policy.evaluate_request(&request);
                assert_eq!(
                    decision.is_allow(),
                    expected_allow,
                    "{matcher}: {decision:?}"
                );
                assert_eq!(
                    decision.is_ask(),
                    !expected_allow,
                    "{matcher}: {decision:?}"
                );
            }
        }
        let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
            {"source": "mode", "ask": "fetch"},
            {"source": "user", "identity_match": "literal", "allow": {"method": "GET"}}
        ]}))
        .unwrap();
        let mixed = ToolApprovalRequest::from_host_json(json!({
            "tool_name": "fetch", "arguments": {"method": "GET", "http_method": "POST"}
        }))
        .unwrap();
        assert!(policy.evaluate_request(&mixed).is_ask());
    }

    #[test]
    fn literal_mcp_names_are_captured_before_receipt_normalization() {
        for tool_name in ["mcp.saved.read", "saved__read"] {
            let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"source": "mode", "ask": tool_name},
                {"source": "user", "identity_match": "literal", "allow": {"mcp_server": "saved", "mcp_tool": "read"}}
            ]})).unwrap();
            let request = ToolApprovalRequest::from_host_json(
                json!({"tool_name": tool_name, "arguments": {}}),
            )
            .unwrap();
            assert!(policy.evaluate_request(&request).is_allow());
        }
        for field in ["capability", "tool_kind", "side_effect"] {
            let policy = ToolApprovalPolicy::from_host_json(json!({"rules": [
                {"source": "mode", "ask": "unknown_invocation"},
                {"source": "user", "identity_match": "literal", "allow": {(field): "saved"}}
            ]}))
            .unwrap();
            let request = ToolApprovalRequest::from_host_json(json!({
                "tool_name": "unknown_invocation", "arguments": {},
                "policy_decision": {"context": {(field): "saved"}}
            }))
            .unwrap();
            assert!(policy.evaluate_request(&request).is_ask(), "{field}");
        }
    }
}
