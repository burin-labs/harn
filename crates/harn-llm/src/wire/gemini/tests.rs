use super::*;
use crate::request::ThinkingConfig;
use serde_json::{json, Value};
use test_support::gemini_payload;

fn payload() -> crate::request::LlmRequestPayload {
    gemini_payload("gemini-2.5-flash", ThinkingConfig::Disabled)
}

#[test]
fn gemini_history_maps_function_calls_and_tool_results() {
    let mut request = payload();
    request.messages = vec![
        json!({"role":"assistant","content":[{"functionCall":{
            "id":"call_1","name":"lookup","args":{"query":"harn"}
        },"thoughtSignature":"opaque-signature"}]}),
        json!({"role":"tool","name":"lookup","tool_call_id":"call_1","content":"{\"result\":\"ok\"}"}),
    ];
    let contents = generate_content_history(&request).contents;
    assert_eq!(contents[0]["role"], "model");
    assert_eq!(
        contents[0]["parts"][0]["functionCall"],
        json!({"id":"call_1","name":"lookup","args":{"query":"harn"}})
    );
    assert_eq!(
        contents[0]["parts"][0]["thoughtSignature"],
        "opaque-signature"
    );
    assert_eq!(contents[1]["role"], "user");
    assert_eq!(
        contents[1]["parts"][0]["functionResponse"],
        json!({"id":"call_1","name":"lookup","response":{"result":"ok"}})
    );
}

#[test]
fn gemini_history_lowers_fixture_provider_metadata() {
    let mut request = payload();
    request.messages = vec![json!({"role":"assistant","tool_calls":[{
        "id":"call_1","name":"lookup","arguments":{"query":"harn"},
        "provider_metadata":{"gemini":{"thought_signature":"fixture-signature"}}
    }]})];
    let contents = generate_content_history(&request).contents;
    assert_eq!(
        contents[0]["parts"][0]["functionCall"],
        json!({"id":"call_1","name":"lookup","args":{"query":"harn"}})
    );
    assert_eq!(
        contents[0]["parts"][0]["thoughtSignature"],
        "fixture-signature"
    );
}

#[test]
fn gemini_tool_response_filters_private_reasoning_blocks() {
    let mut request = payload();
    request.messages = vec![
        json!({"role":"tool","name":"lookup","tool_call_id":"call_1","content":[
            {"type":"thinking","text":"hidden direct","visibility":"private"},
            {"type":"text","text":"visible direct","visibility":"public"},
            {"type":"tool_result","visibility":"internal","content":[
                {"type":"reasoning","text":"hidden nested","visibility":"private"},
                {"type":"text","text":"visible nested","visibility":"public"}
            ]}
        ]}),
    ];
    let wire = json!(generate_content_history(&request).contents).to_string();
    assert!(wire.contains("visible direct"));
    assert!(wire.contains("visible nested"));
    assert!(!wire.contains("hidden direct"));
    assert!(!wire.contains("hidden nested"));
}

fn tool_turn(content: Value) -> Vec<Value> {
    vec![
        json!({"role":"user","content":"dim the lights"}),
        json!({"role":"assistant","content":[{"type":"tool_call","id":"call_abc",
            "name":"set_light","arguments":{"brightness":20},"thought_signature":"sig-xyz"}]}),
        json!({"role":"tool","name":"set_light","tool_call_id":"call_abc","content":content}),
    ]
}

#[test]
fn stateless_tool_replay_emits_thought_then_call_then_result() {
    let mut request = payload();
    request.messages = tool_turn(json!("{\"ok\":true}"));
    let steps = interactions_history(&request).steps;
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["type"], "user_input");
    assert_eq!(steps[1], json!({"type":"thought","signature":"sig-xyz"}));
    assert_eq!(steps[2]["type"], "function_call");
    assert_eq!(steps[2]["id"], "call_abc");
    assert_eq!(steps[2]["name"], "set_light");
    assert_eq!(steps[2]["arguments"], json!({"brightness":20}));
    assert_eq!(steps[3]["type"], "function_result");
    assert_eq!(steps[3]["call_id"], "call_abc");
    assert_eq!(steps[3]["name"], "set_light");
    assert_eq!(steps[3]["result"][0]["type"], "text");
}

fn empty_text_block_anywhere(step: &Value) -> bool {
    ["content", "result"].iter().any(|field| {
        step[field].as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block["type"] == "text" && block["text"] == "")
        })
    })
}

#[test]
fn an_empty_model_text_part_never_becomes_an_empty_text_block() {
    let mut request = payload();
    request.messages = tool_turn(json!("the greeting must say hello"));
    request.messages[1]["content"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"type":"text","text":""}));
    let steps = interactions_history(&request).steps;
    assert!(!steps.iter().any(|step| step["type"] == "model_output"));
    assert!(!steps.iter().any(empty_text_block_anywhere));
    assert_eq!(steps[1], json!({"type":"thought","signature":"sig-xyz"}));
    assert_eq!(steps[2]["type"], "function_call");
    assert_eq!(steps[3]["type"], "function_result");

    request.messages[1]["content"][0]["text"] = json!("reading it now");
    let with_text = interactions_history(&request).steps;
    let output = with_text
        .iter()
        .find(|step| step["type"] == "model_output")
        .expect("real text survives");
    assert_eq!(output["content"][0]["text"], "reading it now");
}

#[test]
fn a_tool_result_with_no_text_still_carries_text() {
    let mut request = payload();
    request.messages = tool_turn(json!(""));
    let steps = interactions_history(&request).steps;
    assert!(!steps.iter().any(empty_text_block_anywhere));
    let result = steps
        .iter()
        .find(|step| step["type"] == "function_result")
        .expect("silent result survives");
    assert_eq!(result["call_id"], "call_abc");
    assert_eq!(result["result"][0]["type"], "text");
    assert_eq!(result["result"][0]["text"], "{\"result\":\"\"}");
    request.messages[2]["content"] = json!("brightness set to 20");
    let with_text = interactions_history(&request).steps;
    let result = with_text
        .iter()
        .find(|step| step["type"] == "function_result")
        .expect("text result survives");
    assert_eq!(
        result["result"][0]["text"],
        "{\"result\":\"brightness set to 20\"}"
    );
}

#[test]
fn chained_tool_replay_sends_only_the_new_result() {
    let mut request = payload();
    request.previous_response_id = Some("v1_prev".into());
    request.messages = tool_turn(json!("{\"ok\":true}"));
    let steps = interactions_history(&request).steps;
    assert_eq!(
        steps.len(),
        1,
        "server-held history must not be billed twice"
    );
    assert_eq!(steps[0]["type"], "function_result");
    assert_eq!(steps[0]["call_id"], "call_abc");
}

#[test]
fn endpoint_families_preserve_their_tool_choice_vocabulary() {
    for (choice, generate, interactions) in [
        (json!("auto"), json!({"mode":"AUTO"}), json!("auto")),
        (json!("none"), json!({"mode":"NONE"}), json!("none")),
        (json!("required"), json!({"mode":"ANY"}), json!("any")),
        (
            json!({"type":"function","function":{"name":"lookup"}}),
            json!({"mode":"ANY","allowedFunctionNames":["lookup"]}),
            json!({"allowed_tools":{"mode":"any","tools":["lookup"]}}),
        ),
        (
            json!("lookup"),
            json!({"mode":"AUTO"}),
            json!({"allowed_tools":{"mode":"any","tools":["lookup"]}}),
        ),
    ] {
        assert_eq!(
            generate_content_tool_config(Some(&choice)).unwrap()["functionCallingConfig"],
            generate
        );
        assert_eq!(
            interactions_tool_choice(Some(&choice)).unwrap(),
            interactions
        );
    }
    assert!(generate_content_tool_config(None).is_none());
    assert!(interactions_tool_choice(None).is_none());
}
