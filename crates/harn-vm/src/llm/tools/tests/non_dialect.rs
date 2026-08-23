use super::{
    build_assistant_response_message, build_assistant_tool_message, collect_tool_schemas, json,
    normalize_tool_args, sample_tool_registry, validate_tool_args, vm_bool, vm_dict, vm_list,
    vm_str, VmValue,
};
use std::collections::BTreeMap;

#[test]
fn validate_tool_args_enforces_required_parameters() {
    let tools = sample_tool_registry();
    let schemas = collect_tool_schemas(Some(&tools), None);
    for args in [
        json!({"action": "create"}),
        json!({"action": "create", "path": null}),
    ] {
        let error = validate_tool_args("edit", &args, &schemas).expect_err("path is required");
        assert!(error.contains("path"));
    }
    assert!(validate_tool_args(
        "edit",
        &json!({"action": "create", "path": "test.go"}),
        &schemas,
    )
    .is_ok());
    assert!(validate_tool_args("unknown", &json!({}), &schemas).is_ok());
}

fn normalization_registry() -> VmValue {
    let mut parameters = BTreeMap::new();
    parameters.insert(
        "action".to_string(),
        vm_dict(&[
            ("type", vm_str("string")),
            (
                "enum",
                vm_list(vec![vm_str("create"), vm_str("delete_range")]),
            ),
        ]),
    );
    parameters.insert(
        "overwrite".to_string(),
        vm_dict(&[("type", vm_str("bool"))]),
    );
    parameters.insert(
        "ops".to_string(),
        vm_dict(&[("type", vm_str("list")), ("required", vm_bool(false))]),
    );
    vm_dict(&[(
        "tools",
        vm_list(vec![vm_dict(&[
            ("name", vm_str("edit")),
            ("parameters", VmValue::dict(parameters)),
        ])]),
    )])
}

#[test]
fn normalize_tool_args_coerces_schema_guided_scalars_and_lists() {
    let tools = normalization_registry();
    for (raw, expected) in [
        ("True", true),
        ("true", true),
        ("FALSE", false),
        ("false", false),
    ] {
        let normalized = normalize_tool_args("edit", json!({"overwrite": raw}), Some(&tools));
        assert_eq!(normalized["overwrite"], json!(expected));
    }
    let normalized = normalize_tool_args(
        "edit",
        json!({"ops": "[{\"op\":\"replace_range\",\"range_start\":3}]"}),
        Some(&tools),
    );
    assert_eq!(
        normalized["ops"],
        json!([{"op": "replace_range", "range_start": 3}])
    );
    let malformed = "[{\"call_id\":\"1\",tool\":\"look\"";
    let normalized = normalize_tool_args("edit", json!({"ops": malformed}), Some(&tools));
    assert_eq!(normalized["ops"], json!(malformed));
    let conservative = normalize_tool_args(
        "edit",
        json!({"overwrite": "yes", "action": "true"}),
        Some(&tools),
    );
    assert_eq!(conservative["overwrite"], json!("yes"));
    assert_eq!(conservative["action"], json!("true"));
}

#[test]
fn normalize_tool_args_flattens_only_unambiguous_registered_actions() {
    let tools = normalization_registry();
    let normalized = normalize_tool_args(
        "edit",
        json!({"delete_range": {"path": "a.cpp", "range_start": 7, "range_end": 9}}),
        Some(&tools),
    );
    assert_eq!(
        normalized,
        json!({"action": "delete_range", "path": "a.cpp", "range_start": 7, "range_end": 9})
    );
    for arguments in [
        json!({"invent_action": {"path": "a.cpp"}}),
        json!({"delete_range": {"path": "a.cpp"}, "path": "outer.cpp"}),
        json!({"delete_range": "a.cpp"}),
        json!({"delete_range": {"action": "create", "path": "a.cpp"}}),
    ] {
        assert_eq!(
            normalize_tool_args("edit", arguments.clone(), Some(&tools)),
            arguments
        );
    }
}

#[test]
fn normalize_tool_args_unwraps_only_complete_heredoc_values() {
    let body = "fn f(a: u32) u32 {\n    return a << 2;\n}";
    for wrapped in [
        format!("<<EOF\n{body}\nEOF"),
        format!("  <<EOF\n{body}\nEOF\n  "),
    ] {
        let normalized = normalize_tool_args("edit", json!({"content": wrapped}), None);
        assert_eq!(normalized["content"], body);
    }
    let nested = normalize_tool_args(
        "edit",
        json!({"ops": [{"new_body": format!("<<EOF\n{body}\nEOF")}]}),
        None,
    );
    assert_eq!(nested["ops"][0]["new_body"], body);

    for value in [
        body.to_string(),
        "<<EOF\nbody\nEOF\ntrailing".to_string(),
        "# Title\n\n```rust\nfn main() {}\n```".to_string(),
    ] {
        let normalized = normalize_tool_args("edit", json!({"content": value.clone()}), None);
        assert_eq!(normalized["content"], value);
    }
}

#[test]
fn assistant_messages_use_one_provider_neutral_shape() {
    let calls = [json!({
        "id": "call_001",
        "name": "read",
        "arguments": {"path": "main.rs"},
        "thought_signature": "tool-signature",
    })];
    let message = build_assistant_tool_message("checking", &calls);
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "checking");
    assert_eq!(
        message["tool_calls"],
        json!([{
            "id": "call_001",
            "name": "read",
            "arguments": {"path": "main.rs"},
            "provider_metadata": {"gemini": {"thought_signature": "tool-signature"}},
        }])
    );
    assert!(!message.to_string().contains("functionCall"));
    assert!(!message.to_string().contains("tool_use"));
}

#[test]
fn assistant_response_message_preserves_reasoning_and_gemini_signatures() {
    let calls = [json!({
        "id": "call_001",
        "name": "read",
        "arguments": {"path": "main.rs"},
        "thought_signature": "tool-signature",
    })];
    let reasoning = build_assistant_response_message("", &[], &calls, Some("inspect first"));
    assert_eq!(reasoning["reasoning"], "inspect first");

    let gemini = build_assistant_response_message(
        "checking",
        &[
            json!({
                "type": "output_text",
                "text": "checking",
                "provider_metadata": {"gemini": {"thought_signature": "text-signature"}},
            }),
            json!({
                "type": "tool_call",
                "id": "call_001",
                "name": "read",
                "arguments": {"path": "main.rs"},
                "thought_signature": "tool-signature",
            }),
        ],
        &calls,
        None,
    );
    assert_eq!(gemini["content"], "checking");
    assert_eq!(
        gemini["tool_calls"][0]["provider_metadata"]["gemini"]["thought_signature"],
        "tool-signature"
    );
}

#[test]
fn local_read_file_honors_offset_and_limit() {
    use super::super::handle_tool_locally;
    use std::io::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("offset.txt");
    let mut file = std::fs::File::create(&path).expect("create fixture");
    for line in 1..=20 {
        writeln!(file, "line {line}").expect("write fixture");
    }
    let path = path.to_str().expect("UTF-8 path");
    let result = handle_tool_locally("read_file", &json!({"path": path, "offset": 5, "limit": 3}))
        .expect("local read");
    assert!(result.contains("5\tline 5"));
    assert!(result.contains("7\tline 7"));
    assert!(!result.contains("8\tline 8"));
    assert!(result.contains("offset=8"));
}
