use super::*;
use crate::schema_compat::test_support::closed_discriminated_union_schema;
use serde_json::json;

fn request() -> LlmRequestPayload {
    let mut request = super::super::openai::test_support::base_request_payload();
    request.provider = "openai".into();
    request.model = "gpt-5.4".into();
    request.api_mode = crate::request::LlmApiMode::Responses;
    request
}

fn encode(request: &LlmRequestPayload) -> super::super::EncodedRequest {
    encode_request(
        request,
        &ResponsesWireContext {
            wire_model: &request.model,
            tool_search_extensions: false,
            reasoning_request: None,
            fast_request: None,
        },
    )
}

#[test]
fn responses_tool_result_screenshot_relocated_to_user_input_image() {
    let mut opts = request();
    opts.messages = vec![
        json!({"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "name": "computer", "arguments": {}}]}),
        json!({"role": "tool", "tool_call_id": "call_1", "content": [
            {"type": "text", "text": "Captured screenshot."},
            {"base64": "AAAB", "media_type": "image/png", "scale_factor": 2.0}
        ]}),
    ];
    let body = encode(&opts).body;
    let items = body["input"].as_array().expect("input items");
    assert_eq!(
        items.len(),
        3,
        "function_call + function_call_output + a user image message"
    );
    assert_eq!(items[0]["type"], "function_call");
    assert_eq!(items[1]["type"], "function_call_output");
    assert_eq!(items[1]["output"], "Captured screenshot.");
    assert!(!items[1]["output"].as_str().unwrap().contains("base64"));
    assert_eq!(items[2]["role"], "user");
    let content = items[2]["content"].as_array().expect("user content");
    let img = content
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("input_image"))
        .expect("input_image present");
    assert_eq!(img["image_url"], "data:image/png;base64,AAAB");
}

#[test]
fn responses_images_share_projection_across_user_and_tool_history() {
    let cases = [
        (
            json!({"type": "image", "base64": "AAAB", "media_type": "image/jpeg"}),
            "data:image/jpeg;base64,AAAB",
        ),
        (
            json!({"type": "input_image", "base64": "AAAB"}),
            "data:image/png;base64,AAAB",
        ),
        (
            json!({"base64": "AAAB", "media_type": "image/png", "scale_factor": 2.0}),
            "data:image/png;base64,AAAB",
        ),
        (
            json!({"type": "image", "url": "https://example.test/image.png"}),
            "https://example.test/image.png",
        ),
        (
            json!({"type": "image_url", "image_url": {"url": "https://example.test/image.png"}}),
            "https://example.test/image.png",
        ),
    ];
    for (image, expected_url) in cases {
        let mut opts = request();
        opts.messages = vec![json!({"role": "user", "content": [image.clone()]})];
        assert_eq!(
            encode(&opts).body["input"],
            json!([{"role": "user", "content": [{"type": "input_image", "image_url": expected_url}]}]),
            "ordinary image: {image}"
        );
        opts.messages = vec![
            json!({"role": "assistant", "tool_calls": [
                {"id": "c1", "name": "camera", "arguments": {}},
                {"id": "c2", "name": "read", "arguments": {}}
            ]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": [
                {"type": "text", "text": "visible result"}, image,
                {"type": "image", "base64": "PRIVATE_IMAGE", "visibility": "private"}
            ]}),
            json!({"role": "tool", "tool_call_id": "c2", "content": "second result"}),
            json!({"role": "assistant", "content": "after images"}),
        ];
        let body = encode(&opts).body;
        let items = body["input"].as_array().expect("input items");
        assert_eq!(items.len(), 6);
        assert_eq!(
            items[2],
            json!({"type": "function_call_output", "call_id": "c1", "output": "visible result"})
        );
        assert_eq!(
            items[3],
            json!({"type": "function_call_output", "call_id": "c2", "output": "second result"})
        );
        assert_eq!(items[4]["role"], "user");
        assert_eq!(
            items[4]["content"][1],
            json!({"type": "input_image", "image_url": expected_url})
        );
        assert_eq!(items[4]["content"].as_array().unwrap().len(), 2);
        assert_eq!(
            items[5],
            json!({"role": "assistant", "content": "after images"})
        );
        assert!(!body.to_string().contains("PRIVATE_IMAGE"));
    }
}

#[test]
fn responses_tool_images_refuse_malformed_base64_without_losing_text() {
    for image in [
        json!({"type": "image", "base64": "", "media_type": "image/png"}),
        json!({"type": "input_image", "base64": ""}),
        json!({"type": "image_url", "base64": ""}),
        json!({"type": "image", "base64": "AAAB", "media_type": ""}),
        json!({"type": "image", "base64": "AAAB", "media_type": false}),
        json!({"type": "image", "base64": "AAAB"}),
    ] {
        let mut opts = request();
        opts.messages = vec![json!({"role": "tool", "tool_call_id": "c1", "content": [
            {"type": "text", "text": "visible result"}, image
        ]})];
        assert_eq!(
            encode(&opts).body["input"],
            json!([{"type": "function_call_output", "call_id": "c1", "output": "visible result"}]),
            "malformed image must not become a provider image: {image}"
        );
    }
}

#[test]
fn responses_input_items_drop_private_reasoning_blocks() {
    let mut opts = request();
    opts.messages = vec![
        json!({"role": "assistant", "content": [
            {"type": "reasoning", "text": "private plan", "visibility": "private"},
            {"type": "output_text", "text": "visible answer", "visibility": "public"}
        ]}),
        json!({"role": "tool", "tool_call_id": "call_1", "content": [
            {"type": "thinking", "text": "hidden tool trace"},
            {"type": "text", "text": "tool output"}
        ]}),
    ];
    let serialized = serde_json::to_string(&encode(&opts).body).expect("serialize");
    assert!(!serialized.contains("private plan"));
    assert!(!serialized.contains("hidden tool trace"));
    assert!(serialized.contains("visible answer"));
    assert!(serialized.contains("tool output"));
}

#[test]
fn responses_projects_canonical_native_tool_history_without_wire_leakage() {
    let mut opts = request();
    opts.messages = vec![
        json!({"role": "assistant", "content": "", "tool_calls": [{
            "id": "toolu_1", "name": "verify", "arguments": {"scope": "tests"}
        }]}),
        json!({"role": "tool_result", "tool_call_id": "toolu_1", "content": "8 checks passed"}),
    ];
    let body = encode(&opts).body;
    assert_eq!(
        body["input"],
        json!([
            {"type": "function_call", "call_id": "toolu_1", "name": "verify", "arguments": "{\"scope\":\"tests\"}"},
            {"type": "function_call_output", "call_id": "toolu_1", "output": "8 checks passed"}
        ])
    );
    let serialized = serde_json::to_string(&body).expect("request serializes");
    assert!(
        !serialized.contains("tool_use"),
        "Anthropic block leaked: {body}"
    );
    assert!(
        !serialized.contains("tool_result"),
        "Anthropic block leaked: {body}"
    );
}

#[test]
fn responses_parallel_tool_results_stay_adjacent_with_image() {
    let mut opts = request();
    opts.messages = vec![
        json!({"role": "assistant", "content": "", "tool_calls": [
            {"id": "c1", "name": "computer", "arguments": {}},
            {"id": "c2", "name": "read", "arguments": {}}
        ]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": [
            {"type": "text", "text": "shot"},
            {"base64": "AAAB", "media_type": "image/png", "scale_factor": 1.0}
        ]}),
        json!({"role": "tool", "tool_call_id": "c2", "content": [{"type": "text", "text": "read ok"}]}),
    ];
    let body = encode(&opts).body;
    let items = body["input"].as_array().expect("input items");
    let kinds: Vec<_> = items
        .iter()
        .map(|item| {
            item.get("type")
                .and_then(|t| t.as_str())
                .or_else(|| item.get("role").and_then(|r| r.as_str()))
                .unwrap_or("")
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output",
            "user"
        ],
        "outputs stay adjacent; image flushes after the run: {items:?}"
    );
}

#[test]
fn responses_builder_projects_sampling_controls_after_admission() {
    let mut opts = request();
    opts.temperature = Some(0.0);
    opts.logprobs = Some(crate::request::LogprobsConfig { top: Some(3) });
    opts.verbosity = Some(crate::request::Verbosity::Low);
    opts.native_tools = Some(vec![json!({"type": "function", "function": {
        "name": "lookup", "description": "Look up a value", "parameters": {"type": "object", "properties": {}}
    }})]);
    opts.parallel_tool_calls = Some(false);
    opts.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::High,
    };
    opts.reasoning_mode = Some("pro".into());
    opts.fast = true;
    let body = encode(&opts).body;
    assert_eq!(body["temperature"], json!(0.0));
    assert_eq!(body["top_logprobs"], 3);
    assert_eq!(body["text"]["verbosity"], "low");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(body["reasoning"].get("mode").is_none());
    assert!(body.get("service_tier").is_none());
    let mode = ReasoningModeRequestDef {
        param_path: vec!["reasoning".into(), "mode".into()],
        value: "pro".into(),
        response_values: vec![],
    };
    let tier = ServingTierRequestDef {
        param: "service_tier".into(),
        value: "priority".into(),
        response_values: vec![],
        beta_header: None,
    };
    let body = encode_request(
        &opts,
        &ResponsesWireContext {
            wire_model: "resolved-wire-model",
            tool_search_extensions: false,
            reasoning_request: Some(&mode),
            fast_request: Some(&tier),
        },
    )
    .body;
    assert_eq!(body["model"], "resolved-wire-model");
    assert_eq!(body["reasoning"], json!({"effort": "high", "mode": "pro"}));
    assert_eq!(body["service_tier"], "priority");
}

#[test]
fn responses_body_maps_structured_output_and_controls() {
    let mut opts = request();
    opts.previous_response_id = Some("resp_prev".into());
    opts.store = Some(false);
    opts.background = Some(true);
    opts.truncation = Some("auto".into());
    opts.include = Some(vec!["file_search_call.results".into()]);
    opts.max_tool_calls = Some(3);
    opts.output_format = OutputFormat::JsonSchema {
        schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object",
            "properties": {
                "ok": {"type": "string", "pattern": "^ok$", "minLength": 2, "format": "email", "default": "ok"},
                "variant": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
            }, "required": ["ok", "variant"]
        }),
        strict: true,
    };
    opts.output_schema = opts.output_format.schema().cloned();
    opts.provider_tools = vec![json!({"type": "web_search_preview"})];
    opts.native_tools = Some(vec![json!({"type": "function", "function": {
        "name": "lookup", "description": "Lookup a record", "strict": true,
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "pattern": "^harn", "minLength": 1, "default": "harn"},
            "mode": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
        }}
    }})]);
    let encoded = encode(&opts);
    // Both transformations are returned for the native audit sink, exactly once each.
    assert_eq!(encoded.schema_audits.len(), 2);
    let body = encoded.body;
    assert_eq!(body["model"], "gpt-5.4");
    assert_eq!(body["previous_response_id"], "resp_prev");
    assert_eq!(body["store"], false);
    assert_eq!(body["background"], true);
    assert_eq!(body["truncation"], "auto");
    assert_eq!(body["include"][0], "file_search_call.results");
    assert_eq!(body["max_tool_calls"], 3);
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    let schema = &body["text"]["format"]["schema"];
    assert_eq!(schema["properties"]["ok"]["type"], "string");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["ok", "variant"]));
    for key in ["default", "pattern", "minLength", "format"] {
        assert!(schema["properties"]["ok"].get(key).is_none(), "{key}");
    }
    assert!(schema["properties"]["variant"].get("oneOf").is_none());
    assert!(schema["properties"]["variant"]["description"]
        .as_str()
        .expect("oneOf compatibility note")
        .contains("Original JSON Schema `oneOf` constraint omitted"));
    assert_eq!(body["tools"][0]["type"], "web_search_preview");
    assert_eq!(body["tools"][1]["type"], "function");
    assert_eq!(body["tools"][1]["name"], "lookup");
    let schema = &body["tools"][1]["parameters"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["mode", "query"]));
    for key in ["default", "pattern", "minLength"] {
        assert!(schema["properties"]["query"].get(key).is_none(), "{key}");
    }
    assert!(schema["properties"]["mode"].get("oneOf").is_none());
}

#[test]
fn responses_body_preserves_closed_discriminated_union_as_any_of() {
    let mut opts = request();
    opts.output_format = OutputFormat::JsonSchema {
        schema: closed_discriminated_union_schema("oneOf", "const"),
        strict: true,
    };
    opts.output_schema = opts.output_format.schema().cloned();
    let body = encode(&opts).body;
    assert_eq!(
        body["text"]["format"],
        json!({
            "type": "json_schema", "name": "response",
            "schema": closed_discriminated_union_schema("anyOf", "enum"), "strict": true
        })
    );
}

#[test]
fn responses_flattens_named_function_tool_choice() {
    let mut opts = request();
    opts.tool_choice = Some(json!({"type": "function", "function": {"name": "echo_marker"}}));
    let body = encode(&opts).body;
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "name": "echo_marker"})
    );
}

#[test]
fn compact_body_keeps_only_compaction_controls() {
    let mut opts = request();
    opts.system = Some("Preserve actionable state.".into());
    opts.previous_response_id = Some("resp_prev".into());
    opts.output_format = OutputFormat::JsonObject;
    opts.provider_tools = vec![json!({"type": "web_search"})];
    opts.truncation = Some("auto".into());
    opts.max_tool_calls = Some(3);
    let body = encode_compact_request(&opts, &opts.model);
    assert_eq!(body["model"], "gpt-5.4");
    assert_eq!(body["instructions"], "Preserve actionable state.");
    assert_eq!(body["previous_response_id"], "resp_prev");
    for key in ["text", "tools", "truncation", "max_tool_calls"] {
        assert!(body.get(key).is_none(), "{key}");
    }
}
