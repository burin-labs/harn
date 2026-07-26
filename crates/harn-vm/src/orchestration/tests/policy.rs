//! The policy ceiling a run executes under.
//!
//! Capability intersection must never expand privilege — narrowing tools,
//! read-only roots, and the process-sandbox policy to the common set — and the
//! sandbox defaults have to keep package-manager config readable. The rest
//! covers mutation-session normalization, and the active execution-policy stack:
//! unknown bridge builtins and MCP escape hatches are rejected, denials record
//! the gate and capability that fired, and resetting thread-local state clears
//! the stack. `glob_match` is the shared tool-name matcher those gates use.

use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn capability_intersection_rejects_privilege_expansion() {
    let ceiling = CapabilityPolicy {
        tools: vec!["read".to_string()],
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(2),
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        tools: vec!["read".to_string(), "edit".to_string()],
        ..Default::default()
    };
    let error = ceiling.intersect(&requested).unwrap_err();
    assert!(error.contains("host ceiling"));
}

#[test]
fn capability_intersection_narrows_read_only_roots_to_common_set() {
    let ceiling = CapabilityPolicy {
        workspace_roots: vec!["/work".to_string()],
        read_only_roots: vec!["/mnt/memory".to_string(), "/mnt/bundle".to_string()],
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        workspace_roots: vec!["/work".to_string()],
        read_only_roots: vec!["/mnt/memory".to_string(), "/mnt/other".to_string()],
        ..Default::default()
    };
    let merged = ceiling.intersect(&requested).unwrap();
    assert_eq!(merged.read_only_roots, vec!["/mnt/memory".to_string()]);

    // An empty side defers to the other, mirroring workspace_roots.
    let unbounded = CapabilityPolicy::default();
    let merged = unbounded.intersect(&requested).unwrap();
    assert_eq!(
        merged.read_only_roots,
        vec!["/mnt/memory".to_string(), "/mnt/other".to_string()]
    );
}

#[test]
fn capability_intersection_narrows_process_sandbox_policy() {
    let ceiling = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            read_roots: vec!["/opt/sdk".to_string()],
            write_roots: vec!["/opt/cache".to_string()],
        },
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::DeveloperToolchains,
                ProcessSandboxPreset::UserTemp,
            ]),
            read_roots: vec!["/opt/sdk".to_string(), "/opt/other".to_string()],
            write_roots: vec!["/opt/cache".to_string(), "/opt/other-cache".to_string()],
        },
        ..Default::default()
    };

    let merged = ceiling.intersect(&requested).unwrap();
    assert_eq!(
        merged.process_sandbox.presets,
        Some(vec![ProcessSandboxPreset::DeveloperToolchains])
    );
    assert_eq!(
        merged.process_sandbox.read_roots,
        vec!["/opt/sdk".to_string()]
    );
    assert_eq!(
        merged.process_sandbox.write_roots,
        vec!["/opt/cache".to_string()]
    );
}

#[test]
fn process_sandbox_defaults_include_package_manager_config() {
    assert!(
        ProcessSandboxPolicy::default()
            .effective_presets()
            .contains(&ProcessSandboxPreset::PackageManagerConfig),
        "package-manager config roots should be part of the default process sandbox presets"
    );
}

#[test]
fn mutation_session_normalize_fills_defaults() {
    let normalized = MutationSessionRecord::default().normalize();
    assert!(normalized.session_id.starts_with("session_"));
    assert_eq!(normalized.mutation_scope, "read_only");
    assert!(normalized.approval_policy.is_none());
}

#[test]
fn install_current_mutation_session_round_trips() {
    let policy = ToolApprovalPolicy {
        require_approval: vec!["edit*".to_string()],
        ..Default::default()
    };
    install_current_mutation_session(Some(MutationSessionRecord {
        session_id: "session_test".to_string(),
        mutation_scope: "apply_workspace".to_string(),
        approval_policy: Some(policy.clone()),
        ..Default::default()
    }));
    let current = current_mutation_session().expect("session installed");
    assert_eq!(current.session_id, "session_test");
    assert_eq!(current.mutation_scope, "apply_workspace");
    assert_eq!(current.approval_policy.as_ref(), Some(&policy));

    install_current_mutation_session(None);
    assert!(current_mutation_session().is_none());
}

#[test]
fn active_execution_policy_rejects_unknown_bridge_builtin() {
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(1),
        ..Default::default()
    });
    let error = enforce_current_policy_for_bridge_builtin("custom_host_builtin").unwrap_err();
    pop_execution_policy();
    assert!(matches!(
        error,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
}

#[test]
fn active_execution_policy_rejects_mcp_escape_hatch() {
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(1),
        ..Default::default()
    });
    let error = enforce_current_policy_for_builtin("mcp_connect", &[]).unwrap_err();
    pop_execution_policy();
    assert!(matches!(
        error,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
}

#[test]
fn reset_thread_local_state_clears_execution_policy_stack() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });
    assert!(current_execution_policy().is_some());
    crate::reset_thread_local_state();
    assert!(current_execution_policy().is_none());
}

#[test]
fn execution_policy_rejects_unlisted_tool() {
    use crate::agent_events::DenialGate;
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        ..Default::default()
    });
    let denial = enforce_current_policy_for_tool("edit").unwrap_err();
    pop_execution_policy();
    // The refusal is structured: gate identifies the tool ceiling and the
    // reason carries the same text the model sees (#2780).
    assert_eq!(denial.gate, DenialGate::ToolCeiling);
    assert!(denial.capability.is_none());
    assert!(denial.reason.contains("tool ceiling"));
}

#[test]
fn execution_policy_capability_ceiling_records_gate_and_capability() {
    use crate::agent_events::DenialGate;
    let mut tool_annotations = BTreeMap::new();
    tool_annotations.insert(
        "edit".to_string(),
        crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["write_text".to_string()],
            )]),
            ..Default::default()
        },
    );
    push_execution_policy(CapabilityPolicy {
        tools: vec!["edit".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        tool_annotations,
        ..Default::default()
    });
    let denial = enforce_current_policy_for_tool("edit").unwrap_err();
    pop_execution_policy();
    assert_eq!(denial.gate, DenialGate::CapabilityCeiling);
    assert_eq!(denial.capability.as_deref(), Some("workspace.write_text"));
}

#[test]
fn arg_constraint_denial_records_arg_constraint_gate() {
    use crate::agent_events::DenialGate;
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let denial =
        enforce_tool_arg_constraints(&policy, "exec", &serde_json::json!({"command": "rm -rf /"}))
            .unwrap_err();
    assert_eq!(denial.gate, DenialGate::ArgConstraint);
}

#[test]
fn glob_match_patterns() {
    assert!(glob_match("*", "anything"));
    assert!(glob_match("exec*", "exec_at"));
    assert!(glob_match("*_file", "read_file"));
    assert!(!glob_match("exec*", "read_file"));
    assert!(glob_match("read_file", "read_file"));
    assert!(!glob_match("read_file", "write_file"));
}
