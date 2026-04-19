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
