use super::*;

#[test]
fn build_llm_call_result_extracts_balanced_json_payloads() {
    let mut opts = base_opts(vec![json!({"role": "user", "content": "Summarize"})]);
    opts.response_format = Some("json".to_string());
    opts.output_schema = Some(json!({
        "type": "object",
        "properties": {
            "purpose": {"type": "string"}
        }
    }));

    let result = LlmResult {
        text: "Here is the result:\n{\"purpose\":\"cli\"}\nThanks.".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    let data = dict.get("data").expect("parsed data");
    let data_dict = data.as_dict().expect("object data");
    assert_eq!(
        data_dict.get("purpose").map(VmValue::display).as_deref(),
        Some("cli")
    );
}

#[test]
fn build_llm_call_result_uses_output_schema_without_response_format_flag() {
    let mut opts = base_opts(vec![json!({"role": "user", "content": "Summarize"})]);
    opts.output_schema = Some(json!({
        "type": "object",
        "properties": {
            "frameworks": {
                "type": "array",
                "items": {"type": "string"}
            }
        }
    }));

    let result = LlmResult {
        text: "{\"frameworks\":[\"go test\"]}".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    assert!(
        dict.get("data").is_some(),
        "structured output should populate data"
    );
}

#[test]
fn build_llm_call_result_extracts_json_from_tagged_prose() {
    let mut opts = base_opts(vec![json!({"role": "user", "content": "Return JSON"})]);
    opts.output_schema = Some(json!({
        "type": "object",
        "properties": {
            "frameworks": {
                "type": "array",
                "items": {"type": "string"}
            }
        }
    }));

    let result = LlmResult {
        text: "<assistant_prose>{\"frameworks\":[\"cargo nextest\"]}</assistant_prose>".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    assert!(
        dict.get("data").is_some(),
        "tagged prose should still populate structured data"
    );
}

#[test]
fn build_llm_call_result_leaves_plain_text_unflagged_without_tools() {
    let opts = base_opts(vec![json!({"role": "user", "content": "What is 2 + 2?"})]);
    let result = LlmResult {
        text: "4".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 8,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    assert_eq!(
        dict.get("prose").map(VmValue::display).as_deref(),
        Some("4")
    );
    assert!(
        dict.get("protocol_violations").is_none(),
        "plain no-tool llm_call should not be judged against the tagged agent protocol"
    );
}

#[test]
fn build_llm_call_result_unwraps_tagged_no_tool_visible_text() {
    let opts = base_opts(vec![json!({"role": "user", "content": "Say hello"})]);
    let result = LlmResult {
        text: "<assistant_prose>Hello there</assistant_prose>\n<done>##DONE##</done>".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 8,
        output_tokens: 4,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    assert_eq!(
        dict.get("prose").map(VmValue::display).as_deref(),
        Some("Hello there")
    );
    assert_eq!(
        dict.get("visible_text").map(VmValue::display).as_deref(),
        Some("Hello there")
    );
    assert!(
        dict.get("protocol_violations").is_none(),
        "well-formed tagged no-tool responses should not report protocol violations"
    );
}

#[test]
fn build_llm_call_transcript_keeps_private_reasoning_out_of_visible_message_text() {
    let opts = base_opts(vec![json!({"role": "user", "content": "Explain the repo"})]);
    let result = LlmResult {
        text: "Visible answer".to_string(),
        tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 3,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: Some("private chain of thought".to_string()),
        thinking_summary: None,
        stop_reason: None,
        blocks: vec![
            json!({
                "type": "reasoning",
                "text": "private chain of thought",
                "visibility": "private",
            }),
            json!({
                "type": "output_text",
                "text": "Visible answer",
                "visibility": "public",
            }),
        ],
    };

    let vm_result = build_llm_call_result(&result, &opts);
    let dict = vm_result.as_dict().expect("dict");
    let transcript = dict
        .get("transcript")
        .and_then(VmValue::as_dict)
        .expect("transcript");
    let events = match transcript.get("events") {
        Some(VmValue::List(events)) => events,
        _ => panic!("events"),
    };
    let assistant_message = events
        .iter()
        .find(|event| {
            event
                .as_dict()
                .and_then(|dict| dict.get("kind"))
                .map(VmValue::display)
                .as_deref()
                == Some("message")
                && event
                    .as_dict()
                    .and_then(|dict| dict.get("role"))
                    .map(VmValue::display)
                    .as_deref()
                    == Some("assistant")
        })
        .and_then(VmValue::as_dict)
        .expect("assistant message event");
    let text = assistant_message
        .get("text")
        .map(VmValue::display)
        .unwrap_or_default();
    assert_eq!(text, "Visible answer");
    assert!(!text.contains("private chain of thought"));
}
