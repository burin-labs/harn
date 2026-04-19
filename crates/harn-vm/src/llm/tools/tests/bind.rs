use super::*;

#[test]
fn normalize_tool_args_rewrites_declared_aliases_from_active_policy() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    use crate::tool_annotations::{ToolAnnotations, ToolArgSchema, ToolKind};

    let mut annotations = std::collections::BTreeMap::new();
    let mut arg_aliases = std::collections::BTreeMap::new();
    arg_aliases.insert("file".to_string(), "path".to_string());
    arg_aliases.insert("mode".to_string(), "action".to_string());
    annotations.insert(
        "edit".to_string(),
        ToolAnnotations {
            kind: ToolKind::Edit,
            arg_schema: ToolArgSchema {
                arg_aliases,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let policy = CapabilityPolicy {
        tool_annotations: annotations,
        ..Default::default()
    };
    push_execution_policy(policy);

    let out = normalize_tool_args(
        "edit",
        &json!({"file": "lib/foo.rs", "mode": "replace_range", "range_start": "3"}),
    );
    assert_eq!(out["path"], json!("lib/foo.rs"));
    assert_eq!(out["action"], json!("replace_range"));
    assert!(out.get("file").is_none());
    assert!(out.get("mode").is_none());
    assert_eq!(out["range_start"], json!(3));

    pop_execution_policy();
}

#[test]
fn normalize_tool_args_skips_unannotated_tool() {
    let out = normalize_tool_args("mystery_tool", &json!({"file": "x.rs"}));
    assert_eq!(out, json!({"file": "x.rs"}));
}

#[test]
fn normalize_tool_args_coerces_integer_like_string_fields() {
    let normalized = normalize_tool_args(
        "edit",
        &json!({
            "action": "replace_range",
            "path": "tests/unit/test_example.py",
            "range_start": "1",
            "range_end": "19",
            "ops": [
                {"op": "replace_range", "range_start": "3", "range_end": "5"}
            ]
        }),
    );
    assert_eq!(normalized["range_start"], json!(1));
    assert_eq!(normalized["range_end"], json!(19));
    assert_eq!(normalized["ops"][0]["range_start"], json!(3));
    assert_eq!(normalized["ops"][0]["range_end"], json!(5));
}

#[test]
fn validate_tool_args_reports_missing_required_params() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"action": "create"});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_err(), "should report missing required param");
    let msg = result.unwrap_err();
    assert!(msg.contains("path"), "error should mention 'path': {msg}");
    assert!(
        msg.contains("missing required parameter"),
        "error should say missing required: {msg}"
    );
}

#[test]
fn validate_tool_args_passes_when_all_required_present() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"action": "create", "path": "test.go", "content": "pkg main"});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_ok(), "should pass with all required params");
}

#[test]
fn validate_tool_args_skips_unknown_tool() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"foo": "bar"});
    let result = validate_tool_args("nonexistent_tool", &args, &schemas);
    assert!(
        result.is_ok(),
        "should pass for unknown tool (handled elsewhere)"
    );
}

#[test]
fn validate_tool_args_treats_null_as_missing() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let args = json!({"action": "create", "path": null});
    let result = validate_tool_args("edit", &args, &schemas);
    assert!(result.is_err(), "null should count as missing");
    assert!(result.unwrap_err().contains("path"));
}

#[test]
fn validate_tool_args_passes_with_empty_args_when_no_required() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    let result = validate_tool_args("no_such_tool", &json!({}), &schemas);
    assert!(result.is_ok());
}
