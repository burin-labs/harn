use super::*;
use crate::orchestration::{CapabilityPolicy, WorkflowNode};
use std::collections::BTreeMap;

fn ceiling_with_tools(tools: &[&str]) -> CapabilityPolicy {
    CapabilityPolicy {
        tools: tools.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    }
}

fn options_with_policy(policy: &CapabilityPolicy) -> crate::value::DictMap {
    let mut options = crate::value::DictMap::new();
    insert_json_vm_option(&mut options, "policy", policy).unwrap();
    options
}

#[test]
fn ceiling_pass_through_is_within() {
    let ceiling = ceiling_with_tools(&["read", "edit"]);
    // The parity path: flattener passes the ceiling through unchanged.
    assert!(ceiling.assert_within_ceiling(&ceiling).is_ok());
    let options = options_with_policy(&ceiling);
    assert!(enforce_flattened_ceiling(&options, &ceiling).is_ok());
}

#[test]
fn narrowing_is_allowed() {
    let ceiling = ceiling_with_tools(&["read", "edit", "run_command"]);
    let narrowed = ceiling_with_tools(&["read"]);
    assert!(ceiling.assert_within_ceiling(&narrowed).is_ok());
}

#[test]
fn widening_tools_is_rejected() {
    let ceiling = ceiling_with_tools(&["read"]);
    let widened = ceiling_with_tools(&["read", "run_command"]);
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(
        err.contains("run_command"),
        "error names the widened tool: {err}"
    );

    // ... and surfaces as a ToolRejected VmError at the flatten seam.
    let options = options_with_policy(&widened);
    match enforce_flattened_ceiling(&options, &ceiling) {
        Err(VmError::CategorizedError { message, category }) => {
            assert_eq!(category, crate::value::ErrorCategory::ToolRejected);
            assert!(message.contains("run_command"), "message: {message}");
        }
        other => panic!("expected a ToolRejected error, got {other:?}"),
    }
}

#[test]
fn widening_capability_op_is_rejected() {
    let mut ceiling = CapabilityPolicy::default();
    ceiling
        .capabilities
        .insert("fs".to_string(), vec!["read".to_string()]);
    let mut widened = CapabilityPolicy::default();
    widened.capabilities.insert(
        "fs".to_string(),
        vec!["read".to_string(), "write".to_string()],
    );
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(err.contains("fs") && err.contains("write"), "error: {err}");
}

#[test]
fn adding_new_capability_is_rejected() {
    let mut ceiling = CapabilityPolicy::default();
    ceiling
        .capabilities
        .insert("fs".to_string(), vec!["read".to_string()]);
    let mut widened = ceiling.clone();
    widened
        .capabilities
        .insert("net".to_string(), vec!["connect".to_string()]);
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(
        err.contains("net"),
        "error names the added capability: {err}"
    );
}

#[test]
fn widening_recursion_budget_is_rejected() {
    let ceiling = CapabilityPolicy {
        recursion_limit: Some(2),
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        recursion_limit: Some(9),
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&widened).is_err());
    // Dropping the budget entirely is also a widening.
    let dropped = CapabilityPolicy::default();
    assert!(ceiling.assert_within_ceiling(&dropped).is_err());
    // Narrowing the budget is allowed.
    let narrowed = CapabilityPolicy {
        recursion_limit: Some(1),
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&narrowed).is_ok());
}

#[test]
fn widening_roots_is_rejected() {
    let ceiling = CapabilityPolicy {
        workspace_roots: vec!["/repo".to_string()],
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        workspace_roots: vec!["/repo".to_string(), "/etc".to_string()],
        ..Default::default()
    };
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(err.contains("/etc"), "error: {err}");
}

#[test]
fn widening_side_effect_level_is_rejected() {
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        side_effect_level: Some("network".to_string()),
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&widened).is_err());
}

#[test]
fn unknown_side_effect_level_ranks_fail_closed() {
    // Canonical `rank_str` ranks an unrecognized level as `none` (0), so a
    // typo/injected level can never outrank a real ceiling. A ceiling of
    // `none` still rejects a widening to a known-higher level.
    let ceiling = CapabilityPolicy {
        side_effect_level: Some("none".to_string()),
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        side_effect_level: Some("desktop_control".to_string()),
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&widened).is_err());
    // An unknown requested level ranks 0 (== none), so it is within a
    // `none` ceiling rather than fail-open above it.
    let unknown = CapabilityPolicy {
        side_effect_level: Some("teleport".to_string()),
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&unknown).is_ok());
}

#[test]
fn widening_process_sandbox_roots_is_rejected() {
    use crate::orchestration::ProcessSandboxPolicy;
    let ceiling = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            write_roots: vec!["/repo/.cache".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            write_roots: vec!["/repo/.cache".to_string(), "/etc".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(
        err.contains("process_sandbox.write_roots") && err.contains("/etc"),
        "error: {err}"
    );
    // Narrowing (fewer roots) is allowed.
    assert!(ceiling
        .assert_within_ceiling(&CapabilityPolicy::default())
        .is_ok());
}

#[test]
fn injecting_process_sandbox_roots_into_empty_ceiling_is_rejected() {
    use crate::orchestration::ProcessSandboxPolicy;
    // The common default: a stage that never set process_sandbox has EMPTY
    // read/write roots — ZERO extra subprocess FS access (additive grants,
    // no fallback), i.e. MOST restrictive. A flattener injecting any root
    // must be rejected, not waved through as "unbounded".
    let ceiling = CapabilityPolicy::default();
    for (field, requested) in [
        (
            "process_sandbox.read_roots",
            CapabilityPolicy {
                process_sandbox: ProcessSandboxPolicy {
                    read_roots: vec!["/etc".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        (
            "process_sandbox.write_roots",
            CapabilityPolicy {
                process_sandbox: ProcessSandboxPolicy {
                    write_roots: vec!["/etc".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    ] {
        let err = ceiling.assert_within_ceiling(&requested).unwrap_err();
        assert!(
            err.contains(field) && err.contains("/etc"),
            "empty ceiling must reject injected {field}: {err}"
        );
    }
    // Empty requested against empty ceiling stays allowed (∅ ⊆ ∅).
    assert!(ceiling
        .assert_within_ceiling(&CapabilityPolicy::default())
        .is_ok());
}

#[test]
fn widening_process_sandbox_presets_is_rejected() {
    use crate::orchestration::{ProcessSandboxPolicy, ProcessSandboxPreset};
    let ceiling = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![ProcessSandboxPreset::SystemRuntime]),
            ..Default::default()
        },
        ..Default::default()
    };
    let widened = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(err.contains("process_sandbox presets"), "error: {err}");
}

#[test]
fn dropping_tool_arg_constraint_is_rejected() {
    use crate::orchestration::ToolArgConstraint;
    let constraint = ToolArgConstraint {
        tool: "edit".to_string(),
        arg_patterns: vec!["src/**".to_string()],
        arg_key: Some("path".to_string()),
    };
    let ceiling = CapabilityPolicy {
        tool_arg_constraints: vec![constraint],
        ..Default::default()
    };
    // A flattener that drops the scope constraint widens edit to anywhere.
    let widened = CapabilityPolicy::default();
    let err = ceiling.assert_within_ceiling(&widened).unwrap_err();
    assert!(
        err.contains("tool_arg_constraints") && err.contains("edit"),
        "error: {err}"
    );
    // Keeping it (and adding more) is allowed.
    let mut narrowed = ceiling.clone();
    narrowed.tool_arg_constraints.push(ToolArgConstraint {
        tool: "run_command".to_string(),
        arg_patterns: vec!["cargo *".to_string()],
        arg_key: None,
    });
    assert!(ceiling.assert_within_ceiling(&narrowed).is_ok());
}

#[test]
fn weakening_tool_annotation_is_rejected() {
    use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema};
    let strong = ToolAnnotations {
        side_effect_level: SideEffectLevel::ReadOnly,
        arg_schema: ToolArgSchema {
            path_params: vec!["path".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ceiling = CapabilityPolicy {
        tools: vec!["edit".to_string(), "read".to_string()],
        ..Default::default()
    };
    ceiling
        .tool_annotations
        .insert("edit".to_string(), strong.clone());

    // Dropping the annotation for a still-granted tool (loses path_params →
    // path constraint becomes unresolvable/permissive) is a widening.
    let mut dropped = ceiling.clone();
    dropped.tool_annotations.clear();
    let err = ceiling.assert_within_ceiling(&dropped).unwrap_err();
    assert!(
        err.contains("tool_annotations") && err.contains("edit"),
        "error: {err}"
    );

    // Rewriting it (e.g. lowering the side-effect level) is also rejected.
    let mut rewritten = ceiling.clone();
    rewritten.tool_annotations.insert(
        "edit".to_string(),
        ToolAnnotations {
            side_effect_level: SideEffectLevel::None,
            ..strong
        },
    );
    assert!(ceiling.assert_within_ceiling(&rewritten).is_err());

    // But if the flattener narrows the tool set so `edit` is no longer
    // granted, dropping its annotation is fine.
    let narrowed_tools = CapabilityPolicy {
        tools: vec!["read".to_string()],
        ..Default::default()
    };
    assert!(ceiling.assert_within_ceiling(&narrowed_tools).is_ok());
}

/// The pinned pre-move Rust flattening algorithm (the deleted
/// `workflow_stage_agent_loop_options` body + helpers), preserved verbatim
/// as the parity oracle. `flatten_matches_pre_move_rust` asserts the Harn
/// flattener reproduces it dict-for-dict.
fn legacy_flatten_reference(
    node: &WorkflowNode,
    session_id: &str,
    tool_format: &str,
    mut options: crate::value::DictMap,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
) -> crate::value::DictMap {
    if let Some(raw) = node.raw_model_policy.as_ref().and_then(|v| v.as_dict()) {
        for (key, value) in raw {
            if !matches!(value, VmValue::Nil) {
                options.insert(key.clone(), value.clone());
            }
        }
    }
    if !options.contains_key("command_policy") {
        if let Some(command_policy) = node
            .raw_model_policy
            .as_ref()
            .and_then(|v| v.as_dict())
            .and_then(|d| d.get("policy"))
            .and_then(|v| v.as_dict())
            .and_then(|p| p.get("command_policy"))
        {
            options.insert(
                crate::value::intern_key("command_policy"),
                command_policy.clone(),
            );
        }
    }
    if !node.auto_compact.enabled {
        options.insert(
            crate::value::intern_key("auto_compact"),
            VmValue::Bool(false),
        );
    } else {
        options.insert(
            crate::value::intern_key("auto_compact"),
            VmValue::Bool(true),
        );
        if let Some(v) = node.auto_compact.token_threshold {
            options.insert(
                crate::value::intern_key("compact_threshold"),
                VmValue::Int(v as i64),
            );
        }
        if let Some(v) = node.auto_compact.tool_output_max_chars {
            options.insert(
                crate::value::intern_key("tool_output_max_chars"),
                VmValue::Int(v as i64),
            );
        }
        if let Some(v) = node.auto_compact.hard_limit_tokens {
            options.insert(
                crate::value::intern_key("hard_limit_tokens"),
                VmValue::Int(v as i64),
            );
        }
        if let Some(s) = node.auto_compact.compact_strategy.as_ref() {
            options.put_str("compact_strategy", s.clone());
        }
        if let Some(s) = node.auto_compact.hard_limit_strategy.as_ref() {
            options.put_str("hard_limit_strategy", s.clone());
        }
        let raw = node.raw_auto_compact.as_ref().and_then(|v| v.as_dict());
        let keep = raw
            .and_then(|d| d.get("compact_keep_last"))
            .and_then(|v| v.as_int())
            .filter(|v| *v >= 0)
            .or_else(|| {
                raw.and_then(|d| d.get("keep_last"))
                    .and_then(|v| v.as_int())
                    .filter(|v| *v >= 0)
            });
        if let Some(v) = keep {
            options.insert(
                crate::value::intern_key("compact_keep_last"),
                VmValue::Int(v),
            );
        }
        if let Some(p) = raw
            .and_then(|d| d.get("summarize_prompt"))
            .and_then(|v| match v {
                VmValue::String(t) if !t.trim().is_empty() => Some(t.to_string()),
                _ => None,
            })
        {
            options.put_str("summarize_prompt", p);
        }
        if let Some(d) = raw {
            for key in ["compress_callback", "mask_callback"] {
                if let Some(cb) = d.get(key) {
                    options.insert(crate::value::intern_key(key), cb.clone());
                }
            }
            if let Some(cb) = d.get("custom_compactor") {
                options.insert(crate::value::intern_key("compact_callback"), cb.clone());
            }
        }
    }
    if !tool_names.is_empty() {
        if let Some(v) = tools_value.clone() {
            options.insert(crate::value::intern_key("tools"), v);
        }
    }
    let tool_policy = tool_capability_policy_from_spec(&node.tools);
    let effective = tool_policy.intersect(&node.capability_policy).unwrap();
    insert_json_vm_option(&mut options, "policy", &effective).unwrap();
    insert_json_vm_option(&mut options, "approval_policy", &node.approval_policy).unwrap();
    options.put_str("session_id", session_id);
    options.put_str("tool_format", tool_format);
    let label = node.id.clone().unwrap_or_else(|| session_id.to_string());
    crate::orchestration::annotate_nested_execution_options(
        &mut options,
        crate::orchestration::NestedExecutionKind::WorkflowStage,
        &label,
    );
    options
}

fn representative_node() -> WorkflowNode {
    let mut raw_model_policy = BTreeMap::new();
    raw_model_policy.insert(
        "provider".to_string(),
        VmValue::String(arcstr::ArcStr::from("anthropic")),
    );
    raw_model_policy.insert("temperature".to_string(), VmValue::Float(0.2));
    // Nested command policy hoisted to the top level by the flattener.
    let mut nested_policy = BTreeMap::new();
    nested_policy.insert(
        "command_policy".to_string(),
        VmValue::String(arcstr::ArcStr::from("worktree")),
    );
    raw_model_policy.insert("policy".to_string(), VmValue::dict(nested_policy));
    // A nil entry must be skipped by the merge.
    raw_model_policy.insert("nudge".to_string(), VmValue::Nil);

    let mut raw_auto_compact = BTreeMap::new();
    raw_auto_compact.insert("keep_last".to_string(), VmValue::Int(4));
    raw_auto_compact.insert(
        "summarize_prompt".to_string(),
        VmValue::String(arcstr::ArcStr::from("summarize tersely")),
    );

    WorkflowNode {
        id: Some("act".to_string()),
        kind: "stage".to_string(),
        mode: Some("agent".to_string()),
        tools: serde_json::json!(["read", "edit"]),
        auto_compact: crate::orchestration::AutoCompactPolicy {
            enabled: true,
            token_threshold: Some(8000),
            tool_output_max_chars: Some(2000),
            hard_limit_tokens: Some(20000),
            compact_strategy: Some("summary".to_string()),
            hard_limit_strategy: Some("truncate".to_string()),
        },
        capability_policy: CapabilityPolicy {
            tools: vec!["read".to_string(), "edit".to_string()],
            recursion_limit: Some(3),
            ..Default::default()
        },
        raw_model_policy: Some(VmValue::dict(raw_model_policy)),
        raw_auto_compact: Some(VmValue::dict(raw_auto_compact)),
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flatten_matches_pre_move_rust() {
    crate::reset_thread_local_state();
    let node = representative_node();
    let session_id = "session-parity";
    let tool_format = "text";
    let tool_names = vec!["read".to_string(), "edit".to_string()];
    let tools_value = Some(crate::stdlib::json_to_vm_value(&node.tools));

    // Base agent_loop options (as std/workflow/options would normalize).
    let mut base = crate::value::DictMap::new();
    base.insert(
        crate::value::intern_key("loop_until_done"),
        VmValue::Bool(true),
    );
    base.insert(crate::value::intern_key("max_iterations"), VmValue::Int(16));

    let stage_agent_options = super::super::WorkflowStageAgentOptions {
        run_agent_loop: true,
        tool_format: tool_format.to_string(),
        llm_options: BTreeMap::new(),
        agent_loop_options: base
            .iter()
            .map(|(k, v)| (k.to_string(), vm_value_to_json(v)))
            .collect(),
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);

    let flattened = workflow_stage_agent_loop_options(
        &ctx,
        &node,
        session_id,
        &tools_value,
        &tool_names,
        &stage_agent_options,
    )
    .await
    .expect("harn flatten succeeds");

    let expected = legacy_flatten_reference(
        &node,
        session_id,
        tool_format,
        base,
        &tools_value,
        &tool_names,
    );

    let flattened_json = vm_value_to_json(&VmValue::dict(flattened));
    let expected_json = vm_value_to_json(&VmValue::dict(expected));
    assert_eq!(
        flattened_json, expected_json,
        "Harn flatten must be dict-equal to the pre-move Rust flatten"
    );
}
