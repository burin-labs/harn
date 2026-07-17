use std::collections::BTreeMap;

use super::*;

#[test]
fn capability_intersection_preserves_explicit_deny_all() {
    let mut deny_all = CapabilityPolicy::default();
    deny_all.restrict_tools(Vec::new());
    deny_all.restrict_capabilities(BTreeMap::new());
    let unbounded = CapabilityPolicy::default();

    let merged = unbounded.intersect(&deny_all).unwrap();
    assert!(merged.tools_are_restricted());
    assert!(merged.tools_deny_all());
    assert!(merged.capabilities_are_restricted());
    assert!(merged.capabilities_deny_all());

    let merged = deny_all.intersect(&unbounded).unwrap();
    assert!(merged.tools_are_restricted());
    assert!(merged.capabilities_are_restricted());

    let grants = CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("llm".to_string(), vec!["call".to_string()])]),
        ..Default::default()
    };
    assert!(deny_all.intersect(&grants).is_err());
    assert!(deny_all.assert_within_ceiling(&grants).is_err());
}

#[test]
fn malformed_deny_all_encodings_fail_closed_and_recanonicalize() {
    let mut policy = CapabilityPolicy::default();
    policy.restrict_tools(Vec::new());
    policy.tools.push("read".to_string());
    policy.restrict_capabilities(BTreeMap::new());
    policy
        .capabilities
        .insert("llm".to_string(), vec!["call".to_string()]);

    assert!(policy.tools_deny_all());
    assert!(policy.allowed_tool_patterns().next().is_none());
    assert!(!policy.tool_pattern_allows("read"));
    assert!(policy.capabilities_deny_all());
    assert!(policy.allowed_capabilities().next().is_none());
    assert!(policy.capability_operations("llm").is_none());

    policy.restrict_tools(policy.tools.clone());
    policy.restrict_capabilities(policy.capabilities.clone());
    assert_eq!(policy.tools.len(), 1);
    assert_eq!(policy.capabilities.len(), 1);
}

#[test]
fn legacy_restriction_flags_round_trip_without_exposing_the_sentinel() {
    let policy: CapabilityPolicy = serde_json::from_value(serde_json::json!({
        "tools": [],
        "tools_restricted": true,
        "capabilities": {},
        "capabilities_restricted": true,
    }))
    .unwrap();
    assert!(policy.tools_deny_all());
    assert!(policy.capabilities_deny_all());

    let encoded = serde_json::to_value(&policy).unwrap();
    assert_eq!(encoded["tools"], serde_json::json!([]));
    assert_eq!(encoded["tools_restricted"], true);
    assert_eq!(encoded["capabilities"], serde_json::json!({}));
    assert_eq!(encoded["capabilities_restricted"], true);
    assert!(!encoded.to_string().contains("harn:deny-all"));

    let unbounded: CapabilityPolicy = serde_json::from_value(serde_json::json!({
        "tools": [],
        "tools_restricted": false,
        "capabilities": {},
        "capabilities_restricted": false,
    }))
    .unwrap();
    assert!(!unbounded.tools_are_restricted());
    assert!(!unbounded.capabilities_are_restricted());
}

#[test]
fn tool_glob_ceiling_accepts_exact_members_but_not_broader_patterns() {
    let ceiling = CapabilityPolicy {
        tools: vec!["read_*".to_string()],
        ..Default::default()
    };

    let exact = CapabilityPolicy {
        tools: vec!["read_file".to_string()],
        ..Default::default()
    };
    assert_eq!(ceiling.intersect(&exact).unwrap().tools, exact.tools);
    assert!(ceiling.assert_within_ceiling(&exact).is_ok());

    let broader = CapabilityPolicy {
        tools: vec!["read*".to_string()],
        ..Default::default()
    };
    assert!(ceiling.intersect(&broader).is_err());
    assert!(ceiling.assert_within_ceiling(&broader).is_err());
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
    let mut deny_all = CapabilityPolicy::default();
    deny_all.restrict_tools(Vec::new());
    deny_all.restrict_capabilities(BTreeMap::new());
    push_execution_policy(deny_all);
    let tool_denial = enforce_current_policy_for_tool("read").unwrap_err();
    let capability_denial = enforce_current_policy_for_builtin("llm_call", &[]).unwrap_err();
    let network_denials = [
        "http_get",
        "__files_upload",
        "sse_server_send",
        "websocket_server_close",
    ]
    .map(|name| enforce_current_policy_for_builtin(name, &[]).unwrap_err());
    assert!(current_allowed_tool_names().is_empty());
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
    for network_denial in network_denials {
        assert!(matches!(
            network_denial,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ));
    }
}
