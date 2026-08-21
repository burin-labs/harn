use std::collections::BTreeMap;

use super::*;
use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};

fn policy_with_path_annotation(tool: &str, kind: ToolKind) {
    let mut annotations = BTreeMap::new();
    annotations.insert(
        tool.to_string(),
        ToolAnnotations {
            kind,
            side_effect_level: match kind {
                ToolKind::Fetch => SideEffectLevel::Network,
                ToolKind::Execute => SideEffectLevel::ProcessExec,
                ToolKind::Edit | ToolKind::Delete | ToolKind::Move => {
                    SideEffectLevel::WorkspaceWrite
                }
                _ => SideEffectLevel::ReadOnly,
            },
            arg_schema: ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    push_execution_policy(CapabilityPolicy {
        tool_annotations: annotations,
        ..Default::default()
    });
}

#[test]
fn compact_rule_shorthand_deserializes() {
    let rule: PolicyRule = serde_json::from_value(serde_json::json!({
        "deny": {"tool": "read_*", "path": "**/.env"},
        "reason": "secret file"
    }))
    .expect("rule");
    assert_eq!(rule.action, PolicyAction::Deny);
    assert_eq!(rule.matches.tool, vec!["read_*"]);
    assert_eq!(rule.matches.path, vec!["**/.env"]);
    assert_eq!(rule.reason.as_deref(), Some("secret file"));
}

#[test]
fn ambiguous_or_invalid_rule_shapes_are_rejected() {
    let invalid_action = serde_json::from_value::<PolicyRule>(serde_json::json!({
        "action": "maybe",
        "match": {"tool": "read_file"}
    }));
    assert!(invalid_action.is_err());

    let mixed_matchers = serde_json::from_value::<PolicyRule>(serde_json::json!({
        "action": "deny",
        "match": {"tool": "read_file"},
        "path": "**/.env"
    }));
    assert!(mixed_matchers.is_err());
}

#[test]
fn deny_beats_ask_and_allow_regardless_of_order() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "rules": [
            {"allow": {"tool": "write_file"}},
            {"ask": {"tool": "write_*"}},
            {"deny": {"tool": "write_file"}, "reason": "blocked"}
        ]
    }))
    .expect("policy");
    let decision =
        evaluate_tool_approval_policy(&policy, "write_file", &serde_json::json!({}), None);
    assert!(decision.is_deny());
    assert_eq!(decision.reason, "blocked");
    assert_eq!(
        decision.matched_rule.as_ref().and_then(|rule| rule.index),
        Some(2)
    );
}

#[test]
fn sensitive_paths_are_denied_by_default() {
    policy_with_path_annotation("read_file", ToolKind::Read);
    let policy = ToolApprovalPolicy::default();
    let decisions = [
        "config/.env",
        ".env.local",
        "/home/agent/.ssh/id_rsa",
        "/home/agent/.aws/credentials",
        "certificates/client.pem",
        "certificates/client.key",
    ]
    .map(|path| {
        evaluate_tool_approval_policy(
            &policy,
            "read_file",
            &serde_json::json!({"path": path}),
            None,
        )
    });
    pop_execution_policy();

    for decision in decisions {
        assert!(decision.is_deny(), "sensitive path was allowed");
        assert!(decision.risk_labels.contains(&"sensitive_path".to_string()));
    }
}

#[test]
fn sensitive_path_default_does_not_classify_edit_content_as_a_path() {
    policy_with_path_annotation("edit_file", ToolKind::Edit);
    let policy = ToolApprovalPolicy::default();
    let decisions = [
        "value = os.environ['API_KEY']\n",
        "const value = process.env.API_KEY;\n",
        "for key in config.keys():\n    print(key)\n",
        "documentation = 'read config/.env before starting'\n",
    ]
    .map(|content| {
        evaluate_tool_approval_policy(
            &policy,
            "edit_file",
            &serde_json::json!({"path": "src/example.py", "content": content}),
            None,
        )
    });
    pop_execution_policy();

    for decision in decisions {
        assert!(
            decision.is_allow(),
            "non-path content was denied: {}",
            decision.reason
        );
    }
}

#[test]
fn sensitive_path_default_uses_path_globs_not_substrings() {
    policy_with_path_annotation("read_file", ToolKind::Read);
    let policy = ToolApprovalPolicy::default();
    let decisions = ["src/os.environment.rs", "src/thing.keyword"].map(|path| {
        evaluate_tool_approval_policy(
            &policy,
            "read_file",
            &serde_json::json!({"path": path}),
            None,
        )
    });
    pop_execution_policy();

    for decision in decisions {
        assert!(
            decision.is_allow(),
            "lookalike path was denied: {}",
            decision.reason
        );
    }
}

#[test]
fn sensitive_path_denial_reason_has_bounded_evidence() {
    policy_with_path_annotation("read_file", ToolKind::Read);
    let long_path = format!("{}/.env", "directory".repeat(128));
    let decision = evaluate_tool_approval_policy(
        &ToolApprovalPolicy::default(),
        "read_file",
        &serde_json::json!({"path": long_path}),
        None,
    );
    pop_execution_policy();

    assert!(decision.is_deny());
    assert!(decision.reason.chars().count() < 300, "{}", decision.reason);
    assert!(decision.reason.contains('…'), "{}", decision.reason);
}

#[test]
fn explicit_sensitive_opt_out_allows_regular_evaluation() {
    let policy = ToolApprovalPolicy {
        allow_sensitive_paths: true,
        ..Default::default()
    };
    let decision = evaluate_tool_approval_policy(
        &policy,
        "read_file",
        &serde_json::json!({"path": "config/.env"}),
        None,
    );
    assert!(decision.is_allow());
    assert!(!decision.has_audit_signal());
}

#[test]
fn external_declared_paths_are_denied_without_root() {
    let temp = tempfile::tempdir().unwrap();
    crate::stdlib::process::set_thread_execution_context(Some(
        crate::orchestration::RunExecutionRecord {
            cwd: Some(temp.path().to_string_lossy().into_owned()),
            project_root: None,
            source_dir: Some(temp.path().to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            environment_policy: Default::default(),
            grants: Vec::new(),
        },
    ));
    policy_with_path_annotation("read_file", ToolKind::Read);
    let decision = evaluate_tool_approval_policy(
        &ToolApprovalPolicy::default(),
        "read_file",
        &serde_json::json!({"path": "/tmp/outside.txt"}),
        None,
    );
    assert!(decision.is_deny());
    assert!(decision.risk_labels.contains(&"external_path".to_string()));
    pop_execution_policy();
    crate::stdlib::process::set_thread_execution_context(None);
}

#[test]
fn path_rule_uses_declared_path_params() {
    policy_with_path_annotation("write_file", ToolKind::Edit);
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{"ask": {"tool": "write_*", "path": "src/**"}, "reason": "source edit"}]
    }))
    .expect("policy");
    let decision = evaluate_tool_approval_policy(
        &policy,
        "write_file",
        &serde_json::json!({"path": "src/lib.rs"}),
        None,
    );
    assert!(decision.is_ask());
    assert_eq!(decision.reason, "source edit");
    pop_execution_policy();
}

#[test]
fn command_url_mcp_identity_and_repeat_rules_match() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [
            {"ask": {"tool": "run_command", "command_identity": "npm"}},
            {"deny": {"tool": "fetch_url", "domain": "*.example.com", "method": "POST"}},
            {"deny": {"mcp_server": "github", "mcp_tool": "create_issue"}},
            {"deny": {"tool": "read_file", "repeat_count_gte": 3}}
        ]
    }))
    .expect("policy");
    assert!(evaluate_tool_approval_policy(
        &policy,
        "run_command",
        &serde_json::json!({"argv": ["npm", "install"]}),
        None,
    )
    .is_ask());
    assert!(evaluate_tool_approval_policy(
        &policy,
        "fetch_url",
        &serde_json::json!({"url": "https://api.example.com/v1", "method": "post"}),
        None,
    )
    .is_deny());
    assert!(evaluate_tool_approval_policy(
        &policy,
        "github__create_issue",
        &serde_json::json!({}),
        None,
    )
    .is_deny());
    assert!(evaluate_tool_approval_policy(
        &policy,
        "read_file",
        &serde_json::json!({"path": "README.md"}),
        Some(3),
    )
    .is_deny());
}

#[test]
fn persona_agent_and_mode_rules_match_args() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{"deny": {"agent": "release-*", "persona": "shipper", "mode": "act"}}]
    }))
    .expect("policy");
    let decision = evaluate_tool_approval_policy(
        &policy,
        "publish",
        &serde_json::json!({"agent": "release-1", "persona": "shipper", "mode": "act"}),
        None,
    );
    assert!(decision.is_deny());
}

#[test]
fn host_request_normalizes_harn_context_and_emits_canonical_receipt() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [
            {
                "id": "allow-fallback",
                "allow": {"tool": "run"}
            },
            {
                "id": "deny-release-publish",
                "deny": {
                    "tool": "run",
                    "tool_kind": "execute",
                    "side_effect": "process_exec",
                    "command_identity": "cargo",
                    "domain": "crates.io",
                    "method": "POST",
                    "agent": "release-*",
                    "persona": "shipper",
                    "mode": "act",
                    "env_mode": "patch",
                    "capability": "terminal.execute"
                },
                "reason": "release publishing is supervised"
            }
        ]
    }))
    .expect("policy");
    let request = ToolApprovalRequest {
        tool_name: "run".to_string(),
        arguments: serde_json::json!({
            "rawInput": {
                "argv": ["cargo", "publish"],
                "url": "https://crates.io/api/v1/crates",
                "method": "post",
                "envMode": "patch"
            }
        }),
        policy_decision: Some(serde_json::json!({
            "context": {
                "toolName": "run",
                "toolKind": "execute",
                "requestedSideEffectLevel": "process_exec",
                "agent_id": "release-1",
                "persona_id": "shipper",
                "action": "act",
                "policy_context": {
                    "capabilities": ["terminal.execute"]
                }
            }
        })),
        approval_request: None,
        repeat_count: None,
    };

    let decision = policy.evaluate_request(&request);

    assert!(decision.is_deny());
    assert_eq!(decision.reason, "release publishing is supervised");
    assert_eq!(
        decision
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.as_deref()),
        Some("deny-release-publish")
    );
    assert_eq!(
        decision.receipt.get("type").and_then(JsonValue::as_str),
        Some(POLICY_RECEIPT_TYPE)
    );
    assert_eq!(
        decision
            .receipt
            .pointer("/context/command_identities/0")
            .and_then(JsonValue::as_str),
        Some("cargo")
    );
}

#[test]
fn host_request_normalizes_generic_argument_paths() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{
            "id": "ask-source-read",
            "ask": {"tool": "read", "path": "src/**"},
            "reason": "source read"
        }]
    }))
    .expect("policy");
    let request = ToolApprovalRequest {
        tool_name: "read".to_string(),
        arguments: serde_json::json!({"path": "src/main.rs"}),
        ..Default::default()
    };

    let decision = policy.evaluate_request(&request);

    assert!(decision.is_ask());
    assert_eq!(decision.reason, "source read");
    assert_eq!(
        decision
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.as_deref()),
        Some("ask-source-read")
    );
}

#[test]
fn write_environment_allows_require_an_exact_mode_match() {
    let wildcard: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{"id": "wildcard", "allow": {"tool": "run", "env_mode": "*"}}]
    }))
    .expect("policy");
    let exact: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{"id": "exact", "allow": {"tool": "run", "env_mode": "patch"}}]
    }))
    .expect("policy");
    let request = ToolApprovalRequest {
        tool_name: "run".to_string(),
        arguments: serde_json::json!({"env_mode": "patch"}),
        ..Default::default()
    };

    let wildcard_decision = wildcard.evaluate_request(&request);
    let exact_decision = exact.evaluate_request(&request);

    assert!(wildcard_decision.is_allow());
    assert!(
        wildcard_decision.matched_rule.is_none(),
        "a wildcard must not grant patch-mode writes"
    );
    assert_eq!(
        exact_decision
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.as_deref()),
        Some("exact")
    );
}

#[test]
fn approval_request_undo_metadata_is_a_supported_host_boundary() {
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [{"id": "ask-network", "ask": {"side_effect": "network"}}]
    }))
    .expect("policy");
    let request = ToolApprovalRequest {
        tool_name: "fetch".to_string(),
        arguments: serde_json::json!({}),
        approval_request: Some(serde_json::json!({
            "undo_metadata": {
                "policy_decision": {
                    "context": {"side_effect": "network"}
                }
            }
        })),
        ..Default::default()
    };

    let decision = policy.evaluate_request(&request);

    assert!(decision.is_ask());
    assert_eq!(
        decision
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.as_deref()),
        Some("ask-network")
    );
}

#[test]
fn approval_unavailable_class_count_uses_sorted_risk_labels() {
    clear_all_approval_policy_repeat_counts();
    let labels = vec![
        "package_install".to_string(),
        "approval_required".to_string(),
    ];
    let reversed = vec![
        "approval_required".to_string(),
        "package_install".to_string(),
    ];

    let (class, count) = next_approval_unavailable_class_repeat_count("s1", &labels);
    assert_eq!(class, "approval_required+package_install");
    assert_eq!(count, 1);
    let (class, count) = next_approval_unavailable_class_repeat_count("s1", &reversed);
    assert_eq!(class, "approval_required+package_install");
    assert_eq!(count, 2);
    let (class, count) = next_approval_unavailable_class_repeat_count("s2", &[]);
    assert_eq!(class, "approval_required");
    assert_eq!(count, 1);

    clear_approval_policy_repeat_counts("s1");
    let (_, count) = next_approval_unavailable_class_repeat_count("s1", &labels);
    assert_eq!(count, 1);
}
