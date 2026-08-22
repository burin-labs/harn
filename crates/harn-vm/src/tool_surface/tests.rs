use super::*;
use crate::orchestration::ToolArgConstraint;
use crate::tool_annotations::ToolArgSchema;

fn execute_annotations() -> ToolAnnotations {
    ToolAnnotations {
        kind: ToolKind::Execute,
        side_effect_level: SideEffectLevel::ProcessExec,
        emits_artifacts: true,
        ..ToolAnnotations::default()
    }
}

#[test]
fn tool_policy_preserves_agent_loop_transport_ceiling() {
    let mut annotations = ToolAnnotations {
        kind: ToolKind::Search,
        side_effect_level: SideEffectLevel::ReadOnly,
        ..ToolAnnotations::default()
    };
    annotations
        .capabilities
        .insert("workspace".into(), vec!["read_text".into()]);
    let policy = tool_capability_policy_from_spec(&serde_json::json!({
        "_type": "tool_registry",
        "tools": [
            {
                "name": "look",
                "parameters": {"type": "object"},
                "policy": annotations
            }
        ]
    }));

    assert_eq!(policy.tools, vec!["look".to_string()]);
    assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
    assert!(policy
        .capabilities
        .get("llm")
        .is_some_and(|ops| ops.contains(&"call".to_string())));
    assert!(policy
        .capabilities
        .get("workspace")
        .is_some_and(|ops| ops.contains(&"read_text".to_string())));
}

#[test]
fn tool_policy_preserves_dependency_key_params() {
    let policy = tool_capability_policy_from_spec(&serde_json::json!({
        "_type": "tool_registry",
        "tools": [
            {
                "name": "edit",
                "parameters": {"type": "object"},
                "policy": {
                    "kind": "edit",
                    "side_effect_level": "workspace_write",
                    "arg_schema": {
                        "path_params": ["path"],
                        "dependency_key_params": ["anchor"],
                        "dependency_range_params": [{"start": "range_start", "end": "range_end"}]
                    }
                }
            },
            {
                "name": "edit_direct",
                "parameters": {"type": "object"},
                "policy": {
                    "kind": "edit",
                    "side_effect_level": "workspace_write",
                    "path_params": ["path"],
                    "dependency_key_params": ["old_string"],
                    "dependency_range_params": [{"start": "line"}]
                }
            }
        ]
    }));

    let annotations = policy.tool_annotations.get("edit").unwrap();
    assert_eq!(annotations.arg_schema.path_params, vec!["path".to_string()]);
    assert_eq!(
        annotations.arg_schema.dependency_key_params,
        vec!["anchor".to_string()]
    );
    assert_eq!(annotations.arg_schema.dependency_range_params.len(), 1);
    assert_eq!(
        annotations.arg_schema.dependency_range_params[0].start,
        "range_start"
    );
    assert_eq!(
        annotations.arg_schema.dependency_range_params[0].end,
        "range_end"
    );
    let direct_annotations = policy.tool_annotations.get("edit_direct").unwrap();
    assert_eq!(
        direct_annotations.arg_schema.dependency_key_params,
        vec!["old_string".to_string()]
    );
    assert_eq!(
        direct_annotations.arg_schema.dependency_range_params.len(),
        1
    );
    assert_eq!(
        direct_annotations.arg_schema.dependency_range_params[0].start,
        "line"
    );
    assert_eq!(
        direct_annotations.arg_schema.dependency_range_params[0].end,
        ""
    );
}

#[test]
fn tool_policy_without_capabilities_keeps_capability_ceiling_unspecified() {
    let policy = tool_capability_policy_from_spec(&serde_json::json!({
        "_type": "tool_registry",
        "tools": [
            {
                "name": "look",
                "parameters": {"type": "object"}
            }
        ]
    }));

    assert_eq!(policy.tools, vec!["look".to_string()]);
    assert!(policy.capabilities.is_empty());
    assert!(policy.side_effect_level.is_none());
}

#[test]
fn execute_artifact_tool_requires_reader() {
    let mut policy = CapabilityPolicy::default();
    policy
        .tool_annotations
        .insert("run".into(), execute_annotations());
    let tools = VmValue::dict(std::collections::BTreeMap::<String, VmValue>::from_iter([
        (
            "_type".into(),
            VmValue::String(arcstr::ArcStr::from("tool_registry")),
        ),
        (
            "tools".into(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
                std::sync::Arc::new(crate::value::DictMap::from_iter([
                    (
                        crate::value::intern_key("name"),
                        VmValue::String(arcstr::ArcStr::from("run")),
                    ),
                    (
                        crate::value::intern_key("parameters"),
                        VmValue::dict(crate::value::DictMap::new()),
                    ),
                    (
                        crate::value::intern_key("executor"),
                        VmValue::String(arcstr::ArcStr::from("host_bridge")),
                    ),
                ])),
            )])),
        ),
    ]));
    let report = validate_tool_surface(&ToolSurfaceInput {
        tools: Some(tools),
        policy: Some(policy),
        ..ToolSurfaceInput::default()
    });
    assert!(report.diagnostics.iter().any(|d| {
        d.code == "TOOL_SURFACE_MISSING_RESULT_READER" && d.severity == ToolSurfaceSeverity::Error
    }));
    assert!(!report.valid);
}

#[test]
fn execute_artifact_tool_accepts_inline_escape_hatch() {
    let mut annotations = execute_annotations();
    annotations.inline_result = true;
    let mut policy = CapabilityPolicy::default();
    policy.tool_annotations.insert("run".into(), annotations);
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "run",
            "parameters": {"type": "object"},
        })]),
        policy: Some(policy),
        ..ToolSurfaceInput::default()
    });
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_MISSING_RESULT_READER"));
}

#[test]
fn native_tool_annotations_are_read_from_tool_json() {
    let mut annotations = execute_annotations();
    annotations.inline_result = true;
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "run",
            "parameters": {"type": "object"},
            "annotations": annotations,
        })]),
        ..ToolSurfaceInput::default()
    });
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_MISSING_ANNOTATIONS"));
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_MISSING_RESULT_READER"));
}

#[test]
fn prompt_reference_outside_policy_is_reported() {
    let policy = CapabilityPolicy {
        tools: vec!["read_file".into()],
        ..CapabilityPolicy::default()
    };
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![
            serde_json::json!({"name": "read_file", "parameters": {"type": "object"}}),
            serde_json::json!({"name": "run_command", "parameters": {"type": "object"}}),
        ]),
        policy: Some(policy),
        prompt_texts: vec!["Use run_command({command: \"cargo test\"})".into()],
        ..ToolSurfaceInput::default()
    });
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_PROMPT_TOOL_NOT_IN_POLICY"));
}

#[test]
fn approval_rule_tool_references_are_reported() {
    let approval_policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "rules": [
            {"ask": {"tool": "missing_tool"}, "reason": "unknown"},
            {"allow": {"tool": "read_*"}}
        ]
    }))
    .unwrap();
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "read_file",
            "parameters": {"type": "object"},
        })]),
        approval_policy: Some(approval_policy),
        ..ToolSurfaceInput::default()
    });

    assert!(report.diagnostics.iter().any(|d| {
        d.code == "TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH"
            && d.field.as_deref() == Some("approval_policy.rules[0].tool")
    }));
    assert!(!report.diagnostics.iter().any(|d| {
        d.code == "TOOL_SURFACE_APPROVAL_PATTERN_NO_MATCH"
            && d.field.as_deref() == Some("approval_policy.rules[1].tool")
    }));
}

#[test]
fn prompt_suppression_ignores_examples() {
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "read_file",
            "parameters": {"type": "object"},
        })]),
        prompt_texts: vec![
            "```text\nrun_command({command: \"old\"})\n```\n<!-- harn-tool-surface: ignore-next-line -->\nrun_command({command: \"old\"})".into(),
        ],
        ..ToolSurfaceInput::default()
    });
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_UNKNOWN_PROMPT_TOOL"));
}

#[test]
fn deprecated_alias_warnings_are_scoped_to_matching_tool_calls() {
    let mut edit_annotations = ToolAnnotations::default();
    edit_annotations
        .arg_schema
        .arg_aliases
        .insert("file".into(), "path".into());
    let mut look_annotations = ToolAnnotations::default();
    look_annotations
        .arg_schema
        .arg_aliases
        .insert("path".into(), "file".into());

    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![
            serde_json::json!({
                "name": "edit",
                "parameters": {"type": "object"},
                "annotations": edit_annotations,
            }),
            serde_json::json!({
                "name": "look",
                "parameters": {"type": "object"},
                "annotations": look_annotations,
            }),
        ]),
        prompt_texts: vec![
            "Use edit({ path: \"src/main.rs\", action: \"replace\" }) before look({ file: \"src/main.rs\" }).".into(),
        ],
        ..ToolSurfaceInput::default()
    });

    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
}

#[test]
fn deprecated_alias_warnings_still_report_matching_multiline_calls() {
    let mut annotations = ToolAnnotations::default();
    annotations
        .arg_schema
        .arg_aliases
        .insert("file".into(), "path".into());

    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "edit",
            "parameters": {"type": "object"},
            "annotations": annotations,
        })]),
        prompt_texts: vec!["Use edit({\n  file: \"src/main.rs\"\n}) once.".into()],
        ..ToolSurfaceInput::default()
    });

    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
}

#[test]
fn deprecated_alias_warnings_report_tagged_text_mode_calls() {
    let mut annotations = ToolAnnotations::default();
    annotations
        .arg_schema
        .arg_aliases
        .insert("file".into(), "path".into());

    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "edit",
            "parameters": {"type": "object"},
            "annotations": annotations,
        })]),
        prompt_texts: vec!["<tool_call>\nedit({ file: \"src/main.rs\" })\n</tool_call>".into()],
        ..ToolSurfaceInput::default()
    });

    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_DEPRECATED_ARG_ALIAS"));
}

#[test]
fn prompt_reference_scanner_tolerates_non_ascii_text() {
    let references = prompt_tool_references("Résumé: use run_command({command: \"test\"})");
    assert!(references.contains("run_command"));
}

#[test]
fn prompt_reference_scanner_reads_tagged_text_mode_calls() {
    let references =
        prompt_tool_references("<tool_call>\nrun({ command: \"cargo test\" })\n</tool_call>");
    assert!(references.contains("run"));
}

#[test]
fn arg_constraint_key_must_exist() {
    let mut annotations = ToolAnnotations {
        kind: ToolKind::Read,
        side_effect_level: SideEffectLevel::ReadOnly,
        arg_schema: ToolArgSchema {
            path_params: vec!["path".into()],
            ..ToolArgSchema::default()
        },
        ..ToolAnnotations::default()
    };
    annotations.arg_schema.required.push("path".into());
    let mut policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "read_file".into(),
            arg_key: Some("missing".into()),
            arg_patterns: vec!["src/**".into()],
        }],
        ..CapabilityPolicy::default()
    };
    policy
        .tool_annotations
        .insert("read_file".into(), annotations);
    let report = validate_tool_surface(&ToolSurfaceInput {
        native_tools: Some(vec![serde_json::json!({
            "name": "read_file",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
        })]),
        policy: Some(policy),
        ..ToolSurfaceInput::default()
    });
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "TOOL_SURFACE_UNKNOWN_ARG_CONSTRAINT_KEY"));
}
