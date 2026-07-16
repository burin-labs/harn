use std::collections::BTreeMap;

use super::*;

#[test]
fn capability_intersection_preserves_explicit_deny_all() {
    let deny_all = CapabilityPolicy {
        tools_restricted: true,
        capabilities_restricted: true,
        ..Default::default()
    };
    let unbounded = CapabilityPolicy::default();

    let merged = unbounded.intersect(&deny_all).unwrap();
    assert!(merged.tools_are_restricted());
    assert!(merged.tools.is_empty());
    assert!(merged.capabilities_are_restricted());
    assert!(merged.capabilities.is_empty());

    let merged = deny_all.intersect(&unbounded).unwrap();
    assert!(merged.tools_are_restricted());
    assert!(merged.capabilities_are_restricted());
}

#[test]
fn capability_intersection_narrows_wildcard_operations() {
    let ceiling = CapabilityPolicy {
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        capabilities: BTreeMap::from([("workspace".to_string(), Vec::new())]),
        ..Default::default()
    };

    let merged = ceiling.intersect(&requested).unwrap();
    assert_eq!(
        merged.capabilities["workspace"],
        vec!["read_text".to_string()]
    );
}

#[test]
fn ceiling_rejects_dropped_legacy_allowlists() {
    let ceiling = CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    };

    assert!(ceiling
        .assert_within_ceiling(&CapabilityPolicy {
            capabilities: ceiling.capabilities.clone(),
            ..Default::default()
        })
        .is_err());
    assert!(ceiling
        .assert_within_ceiling(&CapabilityPolicy {
            tools: ceiling.tools.clone(),
            ..Default::default()
        })
        .is_err());
}

#[test]
fn execution_policy_explicit_empty_allowlists_deny_everything() {
    push_execution_policy(CapabilityPolicy {
        tools_restricted: true,
        capabilities_restricted: true,
        ..Default::default()
    });
    let tool_denial = enforce_current_policy_for_tool("read").unwrap_err();
    let capability_denial = enforce_current_policy_for_builtin("llm_call", &[]).unwrap_err();
    pop_execution_policy();

    assert_eq!(
        tool_denial.gate,
        crate::agent_events::DenialGate::ToolCeiling
    );
    assert!(matches!(
        capability_denial,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
}
