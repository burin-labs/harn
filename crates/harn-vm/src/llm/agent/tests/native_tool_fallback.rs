use super::*;

fn read_tool_registry() -> VmValue {
    let mut tool_params = std::collections::BTreeMap::new();
    tool_params.insert(
        "path".to_string(),
        VmValue::Dict(Rc::new(std::collections::BTreeMap::from([(
            "type".to_string(),
            VmValue::String(Rc::from("string")),
        )]))),
    );
    let tool = VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
        ("name".to_string(), VmValue::String(Rc::from("read"))),
        (
            "description".to_string(),
            VmValue::String(Rc::from("Read a file.")),
        ),
        (
            "parameters".to_string(),
            VmValue::Dict(Rc::new(tool_params)),
        ),
    ])));
    VmValue::Dict(Rc::new(std::collections::BTreeMap::from([(
        "tools".to_string(),
        VmValue::List(Rc::new(vec![tool])),
    )])))
}

fn text_tool_call_response() -> String {
    "<tool_call>\nread({ path: \"src/lib.rs\" })\n</tool_call>".to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn allow_once_accepts_first_native_text_fallback_and_rejects_second() {
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: text_tool_call_response(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: text_tool_call_response(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "inspect the source tree",
    })]);
    opts.tools = Some(read_tool_registry());
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;
    config.tool_format = "native".to_string();
    config.native_tool_fallback = crate::orchestration::NativeToolFallbackPolicy::AllowOnce;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["trace"]["native_text_tool_fallbacks"], json!(2));
    assert_eq!(
        result["trace"]["native_text_tool_fallback_rejections"],
        json!(1)
    );

    let events = result["transcript"]["events"]
        .as_array()
        .expect("transcript events array");
    let fallback_events = events
        .iter()
        .filter(|event| event["kind"] == "native_tool_fallback")
        .collect::<Vec<_>>();
    assert_eq!(fallback_events.len(), 2);
    assert_eq!(fallback_events[0]["metadata"]["accepted"], json!(true));
    assert_eq!(fallback_events[1]["metadata"]["accepted"], json!(false));
    assert!(events.iter().any(|event| {
        event["kind"] == "message"
            && event["text"]
                .as_str()
                .unwrap_or("")
                .contains("native tool mode")
    }));

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn reject_policy_blocks_first_native_text_fallback() {
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: text_tool_call_response(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "inspect the source tree",
    })]);
    opts.tools = Some(read_tool_registry());
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 1;
    config.tool_format = "native".to_string();
    config.native_tool_fallback = crate::orchestration::NativeToolFallbackPolicy::Reject;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["trace"]["native_text_tool_fallbacks"], json!(1));
    assert_eq!(
        result["trace"]["native_text_tool_fallback_rejections"],
        json!(1)
    );
    let events = result["transcript"]["events"]
        .as_array()
        .expect("transcript events array");
    let fallback_event = events
        .iter()
        .find(|event| event["kind"] == "native_tool_fallback")
        .expect("native_tool_fallback event");
    assert_eq!(fallback_event["metadata"]["accepted"], json!(false));

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn empty_completion_retry_is_counted_in_trace_summary() {
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: String::new(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: Some(crate::llm::mock::MockError {
            category: crate::value::ErrorCategory::ServerError,
            message: "openai-compatible model mock reported completion_tokens=1 but delivered no content, reasoning, or tool calls".to_string(),
            retry_after_ms: None,
        }),
    });
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "<done>##DONE##</done>".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "finish this task",
    })]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 1;
    config.llm_retries = 1;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["trace"]["empty_completion_retries"], json!(1));

    reset_llm_mock_state();
}
