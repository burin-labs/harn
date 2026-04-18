use super::*;

#[test]
fn merge_agent_loop_policy_narrows_to_ceiling() {
    push_execution_policy(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("workspace_write".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string(), "read_text".to_string()],
        )]),
        ..Default::default()
    });
    // Request a higher side-effect level but only a subset of capabilities.
    let merged = merge_agent_loop_policy(Some(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("process_exec".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string()],
        )]),
        ..Default::default()
    }))
    .expect("merged policy")
    .expect("policy present");
    pop_execution_policy();

    // Side-effect level narrowed to the ceiling's lower level.
    assert_eq!(merged.side_effect_level.as_deref(), Some("workspace_write"));
    // Capabilities narrowed to the requested subset within the ceiling.
    assert_eq!(
        merged.capabilities.get("workspace"),
        Some(&vec!["write_text".to_string()])
    );
}

#[test]
fn merge_agent_loop_policy_rejects_exceeding_capabilities() {
    push_execution_policy(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("workspace_write".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["write_text".to_string()],
        )]),
        ..Default::default()
    });
    let result = merge_agent_loop_policy(Some(crate::orchestration::CapabilityPolicy {
        side_effect_level: Some("process_exec".to_string()),
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        ..Default::default()
    }));
    pop_execution_policy();

    assert!(
        result.is_err(),
        "should reject capabilities outside ceiling"
    );
}

#[test]
fn merge_approval_policy_intersects_with_ambient_ceiling() {
    use crate::llm::agent_tools::merge_agent_loop_approval_policy;
    use crate::orchestration::{pop_approval_policy, push_approval_policy, ToolApprovalPolicy};

    push_approval_policy(ToolApprovalPolicy {
        auto_deny: vec!["shell*".to_string()],
        ..Default::default()
    });
    let merged = merge_agent_loop_approval_policy(Some(ToolApprovalPolicy {
        auto_deny: vec!["fs_delete".to_string()],
        ..Default::default()
    }))
    .expect("policy present");
    pop_approval_policy();

    // Union of deny lists (more restrictive).
    assert!(merged.auto_deny.iter().any(|p| p == "shell*"));
    assert!(merged.auto_deny.iter().any(|p| p == "fs_delete"));
}

#[test]
fn merge_approval_policy_defers_when_only_one_side_present() {
    use crate::llm::agent_tools::merge_agent_loop_approval_policy;
    use crate::orchestration::{current_approval_policy, ToolApprovalPolicy};

    assert!(current_approval_policy().is_none());
    let merged = merge_agent_loop_approval_policy(Some(ToolApprovalPolicy {
        auto_approve: vec!["read*".to_string()],
        ..Default::default()
    }))
    .expect("policy present");
    assert_eq!(merged.auto_approve, vec!["read*".to_string()]);
}
