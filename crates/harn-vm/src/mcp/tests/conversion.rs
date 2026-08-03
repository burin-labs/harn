use super::*;

#[test]
fn vm_value_to_serde_preserves_supported_shapes() {
    let mut map = BTreeMap::new();
    map.insert("key".to_string(), VmValue::Int(42));

    for (value, expected) in [
        (
            VmValue::String(arcstr::ArcStr::from("hello")),
            serde_json::json!("hello"),
        ),
        (VmValue::dict(map), serde_json::json!({"key": 42})),
        (
            VmValue::List(std::sync::Arc::new(vec![VmValue::Int(1), VmValue::Int(2)])),
            serde_json::json!([1, 2]),
        ),
    ] {
        assert_eq!(vm_value_to_serde(&value), expected);
    }
}

#[test]
fn extract_content_text_handles_text_and_non_text_blocks() {
    for (result, expected) in [
        (
            serde_json::json!({
                "content": [{"type": "text", "text": "hello world"}],
                "isError": false
            }),
            "hello world",
        ),
        (
            serde_json::json!({
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "text", "text": "second"}
                ],
                "isError": false
            }),
            "first\nsecond",
        ),
    ] {
        assert_eq!(extract_content_text(&result), expected);
    }

    let fallback = extract_content_text(&serde_json::json!({
        "content": [{"type": "image", "data": "abc"}],
        "isError": false
    }));
    assert!(fallback.contains("image"));
}

#[test]
fn sse_parser_selects_the_matching_json_rpc_response() {
    let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
    let parsed = parse_sse_jsonrpc_body("mock", body, Some(1)).unwrap();
    assert_eq!(parsed["result"]["tools"], serde_json::json!([]));
}

#[test]
fn embedded_input_rejects_unadvertised_methods() {
    let unknown = unsupported_embedded_input_response(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": "custom-1",
        "method": "custom/method",
        "params": {}
    }))
    .expect("rejection");
    assert_eq!(unknown["error"]["code"], serde_json::json!(-32601));
    assert!(unknown["error"].get("data").is_none());
}
