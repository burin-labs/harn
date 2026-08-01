use std::collections::BTreeMap;

use crate::agent_events::DenialGate;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

use super::super::{
    enforce_current_policy_for_builtin, enforce_current_policy_for_capability,
    enforce_current_policy_for_tool,
    enforce_current_policy_for_tool_with_annotations_and_side_effect_grant,
    enforce_current_policy_for_tool_with_side_effect_grant, pop_execution_policy,
    push_execution_policy, CapabilityPolicy,
};

#[test]
fn execution_policy_rejects_process_exec_when_read_only() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("process".to_string(), vec!["exec".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("exec", &[]);
    pop_execution_policy();
    assert!(result.is_err());
}

#[test]
fn execution_policy_allows_llm_call_under_read_only_side_effect_ceiling() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("llm".to_string(), vec!["call".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("llm_call", &[]);
    pop_execution_policy();
    assert!(result.is_ok());
}

#[test]
fn execution_policy_allows_harness_llm_call_under_read_only_side_effect_ceiling() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("llm".to_string(), vec!["call".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Llm,
        "call",
        &[
            crate::value::VmValue::String("prompt".into()),
            crate::value::VmValue::Nil,
            crate::value::VmValue::dict([(
                "provider",
                crate::value::VmValue::String("mock".into()),
            )]),
        ],
    );
    pop_execution_policy();
    assert!(result.is_ok());
}

#[test]
fn execution_policy_rejects_llm_call_without_llm_capability() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("network".to_string()),
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("llm_call", &[]);
    pop_execution_policy();
    assert!(result.is_err());
}

#[test]
fn side_effect_ceiling_grant_is_exact_to_the_denied_tool_and_effect() {
    let mut tool_annotations = BTreeMap::new();
    tool_annotations.insert(
        "exec".to_string(),
        ToolAnnotations {
            side_effect_level: SideEffectLevel::ProcessExec,
            ..Default::default()
        },
    );
    tool_annotations.insert(
        "fetch".to_string(),
        ToolAnnotations {
            side_effect_level: SideEffectLevel::Network,
            ..Default::default()
        },
    );
    push_execution_policy(CapabilityPolicy {
        tools: vec!["exec".to_string(), "fetch".to_string()],
        side_effect_level: Some("read_only".to_string()),
        tool_annotations,
        ..Default::default()
    });

    let exec_denial = enforce_current_policy_for_tool("exec").unwrap_err();
    assert_eq!(exec_denial.gate, DenialGate::SideEffectCeiling);
    let grant = exec_denial
        .side_effect_grant_for("exec")
        .expect("side-effect denial produces an exact one-call grant");
    assert!(enforce_current_policy_for_tool_with_side_effect_grant("exec", Some(&grant)).is_ok());

    let fetch_denial =
        enforce_current_policy_for_tool_with_side_effect_grant("fetch", Some(&grant)).unwrap_err();
    pop_execution_policy();
    assert_eq!(fetch_denial.gate, DenialGate::SideEffectCeiling);
    assert_eq!(
        fetch_denial
            .side_effect_ceiling
            .expect("typed side-effect details")
            .required_level,
        SideEffectLevel::Network
    );
}

#[test]
fn dispatch_catalog_annotations_fill_an_unannotated_mode_ceiling() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..CapabilityPolicy::neutral()
    });
    let dispatch_annotations = ToolAnnotations {
        side_effect_level: SideEffectLevel::ProcessExec,
        ..Default::default()
    };

    let denial = enforce_current_policy_for_tool_with_annotations_and_side_effect_grant(
        "run",
        Some(&dispatch_annotations),
        None,
    )
    .unwrap_err();
    pop_execution_policy();

    assert_eq!(denial.gate, DenialGate::SideEffectCeiling);
    assert_eq!(
        denial
            .side_effect_ceiling
            .expect("typed side-effect details")
            .required_level,
        SideEffectLevel::ProcessExec
    );
}
