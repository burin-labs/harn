use super::*;
use harn_provider_catalog::capabilities::ReasoningRoundTripPolicy;

fn encode(opts: &LlmRequestPayload, caps: &Capabilities) -> AnthropicEncodedRequest {
    encode_request(
        opts,
        &AnthropicWireContext {
            capabilities: caps,
            wire_model: &opts.model,
            prompt_cache: super::super::prompt_cache::PromptCacheBreakpoint::NotRequested,
            tool_search_extensions: false,
        },
    )
}

#[test]
fn foreign_tool_history_is_projected_without_mutating_the_transcript() {
    let mut opts = test_support::base_payload();
    opts.messages = vec![
        json!({"role":"user","content":"task"}),
        json!({"role":"assistant","content":" ","private_reasoning":"secret",
            "tool_calls":[{"id":"call:1","function":{"name":"read","arguments":"{\"path\":\"a\"}"},"toolset_name":"files"}]}),
        json!({"role":"tool","tool_call_id":"call:1","content":"found","is_error":false}),
        json!({"role":"user","content":"\n"}),
    ];
    let original = opts.messages.clone();
    let encoded = encode(&opts, &Capabilities::default());
    assert_eq!(opts.messages, original);
    assert_eq!(
        encoded.body["messages"],
        json!([
            {"role":"user","content":"task"},
            {"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"read","input":{"path":"a"},"toolset_name":"files"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"found","is_error":false,"toolset_name":"files"}]}
        ])
    );
}

#[test]
fn signed_reasoning_and_prefill_follow_the_explicit_context() {
    let mut opts = test_support::base_payload();
    opts.messages = vec![json!({"role":"assistant","content":[
        {"type":"thinking","thinking":"private","signature":"signature"},
        {"type":"text","text":"answer"}],"reasoning":"stored"})];
    opts.prefill = Some("{".to_owned());
    let mut caps = Capabilities::default();
    caps.reasoning_round_trip = ReasoningRoundTripPolicy::EchoSigned;
    let signed = encode(&opts, &caps).body;
    caps.reasoning_round_trip = ReasoningRoundTripPolicy::Strip;
    let stripped = encode(&opts, &caps).body;
    assert_eq!(
        signed["messages"][0]["content"][0]["signature"],
        "signature"
    );
    assert!(!stripped["messages"][0].to_string().contains("private"));
    assert_eq!(
        signed["messages"][1],
        json!({"role":"assistant","content":"{"})
    );
    assert!(signed["messages"][0].get("reasoning").is_none());
}

#[test]
fn structured_output_uses_the_resolved_native_or_forced_tool_contract() {
    let mut opts = test_support::base_payload();
    opts.output_format = OutputFormat::JsonObject;
    opts.native_tools.as_mut().unwrap()[0]["input_schema"] =
        json!({"type":"string","default":"removed"});
    let mut caps = Capabilities::default();
    caps.structured_output = Some("native".to_owned());
    let forced = encode(&opts, &caps);
    assert!(forced
        .effects
        .iter()
        .any(|effect| matches!(effect, AnthropicWireEffect::ForcedJsonOverridesTools)));
    assert_eq!(
        forced.body["tool_choice"],
        json!({"type":"tool","name":"json_response"})
    );
    assert_eq!(forced.body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        forced
            .effects
            .iter()
            .map(|effect| match effect {
                AnthropicWireEffect::SchemaAudit(_) => "schema",
                AnthropicWireEffect::ForcedJsonOverridesTools => "warning",
            })
            .collect::<Vec<_>>(),
        vec!["schema", "warning", "schema"]
    );
    caps.allowed_tool_choice_modes = vec!["auto".to_owned(), "none".to_owned()];
    let native = encode(&opts, &caps);
    assert!(!native
        .effects
        .iter()
        .any(|effect| matches!(effect, AnthropicWireEffect::ForcedJsonOverridesTools)));
    assert_eq!(native.body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        native.body["output_config"]["format"]["type"],
        "json_schema"
    );
    assert!(native.body.get("tool_choice").is_none());
}

#[test]
fn tool_choice_forms_and_parallel_override_share_one_wire_contract() {
    let mut opts = test_support::base_payload();
    for (choice, expected) in [
        (json!("auto"), json!({"type":"auto"})),
        (json!("any"), json!({"type":"any"})),
        (json!("required"), json!({"type":"any"})),
        (json!("none"), json!({"type":"none"})),
        (
            json!("read_file"),
            json!({"type":"tool","name":"read_file"}),
        ),
        (
            json!({"type":"function","function":{"name":"read_file"}}),
            json!({"type":"tool","name":"read_file"}),
        ),
        (
            json!({"type":"tool","name":"read_file","disable_parallel_tool_use":true}),
            json!({"type":"tool","name":"read_file","disable_parallel_tool_use":true}),
        ),
    ] {
        opts.tool_choice = Some(choice);
        opts.parallel_tool_calls = None;
        assert_eq!(
            encode(&opts, &Capabilities::default()).body["tool_choice"],
            expected
        );
        opts.parallel_tool_calls = Some(true);
        assert_eq!(
            encode(&opts, &Capabilities::default()).body["tool_choice"]
                ["disable_parallel_tool_use"],
            false
        );
    }
    opts.tool_choice = Some(Value::Null);
    opts.parallel_tool_calls = None;
    assert!(encode(&opts, &Capabilities::default())
        .body
        .get("tool_choice")
        .is_none());
    assert!(!tool_choice_forces_tool_use(&json!("none")));
    assert!(tool_choice_forces_tool_use(&json!("required")));
}
