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

/// The agent-session control plane: Harn-owned transcript and session state,
/// including opening the session a model call is recorded in, anchoring its
/// workspace, forking it, and closing it. Every entry declares only `state.*`
/// effects.
///
/// Only the `write` / `mutate` half is enumerated, because only that half is
/// ranked above `read_only` on the side-effect ladder. A read-only sibling
/// needs no marker and gets none — a marker that changed nothing would read
/// as audited while asserting nothing.
///
/// Two families are deliberately out:
///
///   * `harness.agent.state_*` — the DURABLE agent-state namespace. Its first
///     argument is an fs-backed `resource` handle minted by `state_init` /
///     `state_resume`, which declare `fs.mutate` and stay capped. Different
///     resource, different owner.
///   * `harness.agent.worker_*` / `pool_*` — delegated workers, which carry
///     `worker.*` effects. Spawning a sub-agent is a real resource
///     escalation, not loop bookkeeping.
///   * `harness.agent.add_root` / `remove_root` — workspace-topology changes
///     that observe a caller-supplied path. Their contracts declare `fs.read`
///     as well as session state, so the user-world ladder continues to govern
///     them rather than treating a workspace-authority change as bookkeeping.
///
/// `seed_from_jsonl` is out for the same reason: it reads an arbitrary file.
fn agent_session_control_plane_writes() -> Vec<&'static str> {
    const DURABLE_STATE_NAMESPACE: &[&str] = &[
        "state_write",
        "state_read",
        "state_list",
        "state_delete",
        "state_handoff",
    ];
    // `all_builtin_manifest()` and not `all_builtin_defs()`. The latter holds
    // only the `#[harn_builtin]`-emitted VM defs, which is 17 of these 44. The
    // contract-only `capability_method!` declarations are absent from it, so
    // a census built on it silently cannot see most of the methods it exists
    // to police. The manifest is the union the enforcement index is built
    // from, so this censuses exactly what `contract_effect_allowed_by_ceiling`
    // will later consult. Aliases repeat a primary's contract under a second
    // name; only the canonical entry owns the capability method.
    let mut names: Vec<&'static str> = crate::stdlib::all_builtin_manifest()
        .iter()
        .filter(|entry| entry.is_canonical())
        .filter_map(|entry| match entry.contract.exposure {
            harn_builtin_meta::BuiltinExposure::HarnessMethod {
                capability: harn_builtin_meta::CapabilityId::Agent,
                method,
            } => Some((method, entry.contract.effects)),
            _ => None,
        })
        .filter(|(method, effects)| {
            !effects.is_empty()
                && effects
                    .iter()
                    .all(|spec| spec.kind == harn_builtin_meta::EffectKind::State)
                && effects.iter().any(|spec| {
                    matches!(
                        spec.access,
                        harn_builtin_meta::EffectAccess::Write
                            | harn_builtin_meta::EffectAccess::Mutate
                    )
                })
                && !DURABLE_STATE_NAMESPACE.contains(method)
        })
        .map(|(method, _)| method)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Falsifier for the served-path blocker. `harn serve api` runs a turn under
/// the session mode's autonomy tier, and every mode except `code` installs a
/// `read_only` side-effect ceiling. Judged on that ladder,
/// `state:mutate (agent-sessions)` ranks `workspace_write`, so the loop's own
/// `harness.agent.open` was rejected before the first model call and every
/// served turn died. `harn run` never saw it: it installs no ceiling at all.
///
/// Disarm by deleting `runtime_control_plane` from `harness.agent.open` in
/// `crates/harn-capability-contracts/src/ai.rs`; this fails, and the negative
/// control below pins the exact message it fails with.
#[test]
fn agent_session_control_plane_survives_a_read_only_ceiling() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    });
    let session = crate::value::VmValue::String("session-1".into());
    let results: Vec<(&str, Result<(), crate::value::VmError>)> =
        agent_session_control_plane_writes()
            .into_iter()
            .map(|method| {
                let args = [
                    session.clone(),
                    session.clone(),
                    session.clone(),
                    session.clone(),
                ];
                (
                    method,
                    enforce_current_policy_for_capability(
                        harn_builtin_meta::CapabilityId::Agent,
                        method,
                        &args,
                    ),
                )
            })
            .collect();
    pop_execution_policy();

    assert!(
        results.len() >= 40,
        "the control-plane census collapsed to {} entries; a census that measured \
         nothing would pass this test vacuously",
        results.len()
    );
    let rejected: Vec<String> = results
        .iter()
        .filter_map(|(method, result)| {
            result
                .as_ref()
                .err()
                .map(|error| format!("{method}: {error:?}"))
        })
        .collect();
    assert!(
        rejected.is_empty(),
        "the agent loop must be able to run its own session control plane under the \
         ceiling every non-`code` session mode installs; {} of {} rejected:\n  {rejected:#?}",
        rejected.len(),
        results.len(),
    );
}

/// Negative control. Disarm the marker and the same ceiling must still reject
/// the same call, with the message the served path actually reported.
#[test]
fn agent_open_without_the_marker_is_rejected_by_the_ladder() {
    let contract = crate::stdlib::capability_method_manifest_entry(
        harn_builtin_meta::CapabilityId::Agent,
        "open",
    )
    .expect("declared agent.open contract")
    .contract;
    let disarmed = harn_builtin_meta::BuiltinContract::harness(
        harn_builtin_meta::CapabilityId::Agent,
        "open",
        contract.effects,
    );
    let effect = runtime_effects_from_contract(contract.effects, &[])
        .into_iter()
        .next()
        .expect("agent.open declares an effect");
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };

    assert!(
        super::super::contract_effect_allowed_by_ceiling(&effect, contract, &ceiling),
        "the shipped contract must be admitted, or the control below proves nothing"
    );
    assert!(
        !super::super::contract_effect_allowed_by_ceiling(&effect, disarmed, &ceiling),
        "without the marker the ladder must still reject the agent-session write"
    );

    push_execution_policy(ceiling);
    let live = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Agent,
        "state_write",
        &[crate::value::VmValue::String("durable-handle".into())],
    );
    pop_execution_policy();
    let text = format!("{:?}", live.expect_err("durable state stays capped"));
    assert!(
        text.contains("exceeds the active effect ceiling"),
        "unexpected rejection text: {text}"
    );
}

/// The control that keeps the marker honest: it exempts the agent-session
/// control plane from the ladder and NOTHING else about state.
///
/// The durable agent-state namespace is the sharpest possible sibling — same
/// capability handle, same effect kind, same accesses, same ceiling. Only the
/// declaration differs, because its first argument is an fs-backed `resource`
/// handle rather than a session id. If this ever passes, the fix has been read
/// as "state is open now" and the ceiling has stopped meaning anything.
#[test]
fn the_marker_does_not_open_durable_agent_state() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    });
    let handle = crate::value::VmValue::String("durable-handle".into());
    let admitted =
        enforce_current_policy_for_capability(harn_builtin_meta::CapabilityId::Agent, "open", &[]);
    let outcomes: Vec<(&str, bool)> = [
        "state_init",
        "state_resume",
        "state_write",
        "state_delete",
        "state_handoff",
    ]
    .into_iter()
    .map(|method| {
        let args = [handle.clone(), handle.clone(), handle.clone()];
        (
            method,
            enforce_current_policy_for_capability(
                harn_builtin_meta::CapabilityId::Agent,
                method,
                &args,
            )
            .is_err(),
        )
    })
    .collect();
    pop_execution_policy();

    assert!(
        admitted.is_ok(),
        "the session control plane must be admitted in the same breath, or this \
         control proves nothing: {admitted:?}"
    );
    let leaked: Vec<&str> = outcomes
        .iter()
        .filter(|(_, refused)| !refused)
        .map(|(method, _)| *method)
        .collect();
    assert!(
        leaked.is_empty(),
        "`runtime_control_plane` must not become a durable-state write grant; \
         these were admitted under a read_only ceiling: {leaked:#?}"
    );
}

/// Changing the set of mounted workspace roots is not mere bookkeeping. The
/// implementation observes a caller-supplied path before changing session
/// state, so both effects must remain visible and the user-world ladder must
/// continue to govern the operation.
#[test]
fn workspace_topology_changes_are_not_runtime_control_plane() {
    for method in ["add_root", "remove_root"] {
        let contract = crate::stdlib::capability_method_manifest_entry(
            harn_builtin_meta::CapabilityId::Agent,
            method,
        )
        .expect("declared workspace-root method")
        .contract;
        assert!(
            !contract.is_runtime_control_plane(),
            "{method} observes a caller-supplied path and must stay governed by the ladder"
        );
        assert!(
            contract.effects.iter().any(|effect| {
                effect.kind == harn_builtin_meta::EffectKind::Fs
                    && effect.access == harn_builtin_meta::EffectAccess::Read
            }),
            "{method} must expose its filesystem observation to capability checks and receipts"
        );
    }

    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    });
    let denied = enforce_current_policy_for_capability(
        harn_builtin_meta::CapabilityId::Agent,
        "add_root",
        &[
            crate::value::VmValue::String("session-1".into()),
            crate::value::VmValue::String("/outside".into()),
        ],
    );
    pop_execution_policy();
    assert!(
        denied.is_err(),
        "a read-only ceiling must not authorize a workspace-topology change: {denied:?}"
    );
}

/// A capability-restricted ceiling still governs the control plane. The marker
/// relaxes the coarse tool-invasiveness ladder, not the capability gate — a
/// host that says "this run may not write state" must still be obeyed.
#[test]
fn the_marker_does_not_bypass_a_restricted_capability_ceiling() {
    push_execution_policy(CapabilityPolicy {
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });
    let denied =
        enforce_current_policy_for_capability(harn_builtin_meta::CapabilityId::Agent, "open", &[]);
    pop_execution_policy();
    assert!(
        denied.is_err(),
        "a ceiling that withholds state:write must still deny agent.open: {denied:?}"
    );
}

/// Structural guard, not a spot check: a new agent-session control-plane
/// method added without the marker is a new served-path outage, and it would
/// look exactly like the existing methods that carry it. Fail at the registry instead of at
/// a customer's first turn.
#[test]
fn every_agent_session_control_plane_write_declares_the_marker() {
    let census = agent_session_control_plane_writes();
    assert!(
        census.len() >= 40,
        "the control-plane census collapsed to {} entries; it must not read empty \
         and pass vacuously",
        census.len()
    );
    let missing: Vec<&'static str> = census
        .iter()
        .filter(|method| {
            !crate::stdlib::capability_method_manifest_entry(
                harn_builtin_meta::CapabilityId::Agent,
                method,
            )
            .expect("declared agent capability method")
            .contract
            .is_runtime_control_plane()
        })
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "these agent-session control-plane methods write session state but do not \
         declare `runtime_control_plane`, so a served turn under any non-`code` \
         session mode will reject them:\n  {missing:#?}"
    );
}
