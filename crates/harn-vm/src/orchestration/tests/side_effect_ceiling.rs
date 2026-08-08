use std::collections::BTreeMap;

use crate::agent_events::DenialGate;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

use super::super::tool_enforcement::enforce_current_policy_for_tool_with_side_effect_grant;
use super::super::{
    allow_trusted_bridge_calls, enforce_current_policy_for_builtin,
    enforce_current_policy_for_capability, enforce_current_policy_for_tool,
    enforce_current_policy_for_tool_with_annotations_and_side_effect_grant, pop_execution_policy,
    push_execution_policy, runtime_effects_from_contract, CapabilityPolicy, EffectKind,
    EffectScope,
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
fn llm_call_grant_covers_read_only_call_configuration_but_not_generic_state() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("llm".to_string(), vec!["call".to_string()])]),
        ..Default::default()
    });

    let empty_options =
        crate::value::VmValue::dict(BTreeMap::<String, crate::value::VmValue>::new());
    let reasoning = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Llm,
        "apply_reasoning_policy",
        &[empty_options],
    );
    assert!(
        reasoning.is_ok(),
        "an authorized model call must be able to resolve its Harn-owned reasoning policy: {reasoning:?}"
    );

    let runtime_context = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Runtime,
        "context",
        &[],
    );
    assert!(
        runtime_context.is_ok(),
        "agent-loop infrastructure must be able to read its execution-local context: {runtime_context:?}"
    );

    let current_agent = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Agent,
        "current_id",
        &[],
    );
    assert!(
        current_agent.is_ok(),
        "agent-loop infrastructure must be able to read its current session id: {current_agent:?}"
    );

    let unrelated_state = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_get",
        &[crate::value::VmValue::String(
            "unrelated.product.state".into(),
        )],
    );
    pop_execution_policy();
    assert!(
        unrelated_state.is_err(),
        "llm.call must not become a generic state.read grant"
    );
}

#[test]
fn call_configuration_requires_llm_call_authority() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });

    let empty_options =
        crate::value::VmValue::dict(BTreeMap::<String, crate::value::VmValue>::new());
    let reasoning = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Llm,
        "apply_reasoning_policy",
        &[empty_options],
    );
    let runtime_context = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Runtime,
        "context",
        &[],
    );
    let current_agent = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Agent,
        "current_id",
        &[],
    );
    pop_execution_policy();

    assert!(
        reasoning.is_err(),
        "reasoning-policy resolution must not work without llm.call"
    );
    assert!(
        runtime_context.is_err(),
        "the infrastructure context exception must not exist without llm.call"
    );
    assert!(
        current_agent.is_err(),
        "the infrastructure session exception must not exist without llm.call"
    );
}

#[test]
fn call_configuration_and_runtime_context_keep_their_audit_effects() {
    let reasoning = crate::stdlib::capability_method_manifest_entry(
        harn_builtin_meta::CapabilityId::Llm,
        "apply_reasoning_policy",
    )
    .expect("reasoning-policy contract");
    let reasoning_effects = runtime_effects_from_contract(reasoning.contract.effects, &[]);
    assert!(reasoning_effects.iter().any(|effect| {
        matches!(effect.kind, EffectKind::Llm { .. }) && effect.scope == EffectScope::Read
    }));

    let context = crate::stdlib::capability_method_manifest_entry(
        harn_builtin_meta::CapabilityId::Runtime,
        "context",
    )
    .expect("runtime-context contract");
    let context_effects = runtime_effects_from_contract(context.contract.effects, &[]);
    assert!(context_effects.iter().any(|effect| {
        matches!(effect.kind, EffectKind::State) && effect.scope == EffectScope::Read
    }));
    assert_eq!(
        context.contract.effects_authorized_by,
        Some(harn_builtin_meta::EffectAuthorization::new(
            harn_builtin_meta::CapabilityId::Llm,
            "call",
        ))
    );
}

#[test]
fn explicit_llm_catalog_grant_still_admits_configuration_reads() {
    // The llm.call → llm.catalog subsumption must stay one-directional: a
    // policy that grants only the catalog read keeps working without any
    // call authority.
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("llm".to_string(), vec!["catalog".to_string()])]),
        ..Default::default()
    });
    let catalog_read = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Llm,
        "known_models",
        &[],
    );
    let call = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Llm,
        "call",
        &[
            crate::value::VmValue::String("prompt".into()),
            crate::value::VmValue::Nil,
            crate::value::VmValue::Nil,
        ],
    );
    pop_execution_policy();
    assert!(
        catalog_read.is_ok(),
        "an explicit llm.catalog grant keeps working without llm.call: {catalog_read:?}"
    );
    assert!(
        call.is_err(),
        "llm.catalog must not imply model-call authority"
    );
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

#[test]
fn trusted_bridge_depth_exempts_harness_state_reads_like_bridged_builtins() {
    // Tool execution policies often omit `state.read`. Manifest PreToolUse
    // hooks still need session/store reads; invoke_vm_hook_handler raises
    // trusted-bridge depth for exactly that class of first-party entry.
    push_execution_policy(CapabilityPolicy {
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });

    let denied = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_get",
        &[crate::value::VmValue::String(
            "burin.hooks.current_session_id".into(),
        )],
    );
    assert!(
        denied.is_err(),
        "restricted capabilities without state:read must deny store_get"
    );

    let _trusted = allow_trusted_bridge_calls();
    let allowed = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Agent,
        "current_id",
        &[],
    );
    pop_execution_policy();
    assert!(
        allowed.is_ok(),
        "trusted bridge depth must exempt harness methods the same way it exempts bridged builtins"
    );
}
