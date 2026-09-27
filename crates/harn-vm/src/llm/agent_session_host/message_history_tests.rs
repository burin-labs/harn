//! harn#8951: a turn stopped mid-tool leaves an unanswered call, and every
//! later provider request carrying it is refused. These cases pin the repair
//! at the shape level and at each provider's real request body.

use super::{answer_unanswered_tool_calls, UNANSWERED_TOOL_CALL_OBSERVATION};
use crate::llm::api::{LlmCallOptions, LlmRequestPayload};
use crate::llm::providers::{AnthropicProvider, OpenAiCompatibleProvider, OpenAiResponsesProvider};
use serde_json::{json, Value};

fn openai_call(id: &str) -> Value {
    json!({"id": id, "type": "function", "function": {"name": "run", "arguments": "{}"}})
}

fn tool_result(id: &str) -> Value {
    json!({"role": "tool_result", "name": "run", "tool_call_id": id, "content": "ok"})
}

/// The bc#8951 transcript: the call was cancelled, then the person typed again.
fn stopped_mid_tool() -> Vec<Value> {
    vec![
        json!({"role": "user", "content": "run the slow command"}),
        json!({"role": "assistant", "content": "", "tool_calls": [openai_call("call_stopped")]}),
        json!({"role": "user", "content": "try again"}),
    ]
}

fn is_repair_for(message: &Value, id: &str) -> bool {
    message["role"] == "tool_result"
        && message["tool_call_id"] == id
        && message["content"] == UNANSWERED_TOOL_CALL_OBSERVATION
        && message["is_error"] == true
        && message.to_string().contains("harness_repair")
}

#[test]
fn a_call_stopped_before_its_result_is_answered_right_after_its_turn() {
    let repaired = answer_unanswered_tool_calls(&stopped_mid_tool()).expect("one call was open");
    assert_eq!(repaired.len(), 4);
    assert!(
        is_repair_for(&repaired[2], "call_stopped"),
        "{}",
        repaired[2]
    );
    assert_eq!(repaired[3]["content"], "try again");
}

#[test]
fn a_parallel_turn_stopped_between_calls_answers_only_the_open_one() {
    let messages = vec![
        json!({"role": "assistant", "content": "", "tool_calls": [openai_call("a"), openai_call("b")]}),
        tool_result("a"),
        json!({"role": "user", "content": "next"}),
    ];
    let repaired = answer_unanswered_tool_calls(&messages).expect("b was open");
    assert_eq!(repaired.len(), 4);
    assert_eq!(repaired[1]["tool_call_id"], "a");
    assert!(is_repair_for(&repaired[2], "b"), "{}", repaired[2]);
}

#[test]
fn an_anthropic_tool_use_block_is_answered_too() {
    let messages = vec![
        json!({"role": "assistant", "content": [
            {"type": "text", "text": "running"},
            {"type": "tool_use", "id": "toolu_1", "name": "run", "input": {}}
        ]}),
        json!({"role": "user", "content": "next"}),
    ];
    let repaired = answer_unanswered_tool_calls(&messages).expect("toolu_1 was open");
    assert!(is_repair_for(&repaired[1], "toolu_1"), "{}", repaired[1]);
}

#[test]
fn a_well_formed_history_and_an_id_less_call_are_left_alone() {
    let answered = vec![
        json!({"role": "assistant", "content": "", "tool_calls": [openai_call("a")]}),
        tool_result("a"),
    ];
    assert!(answer_unanswered_tool_calls(&answered).is_none());
    // Gemini's `functionCall` id is optional; no provider can pair a result to
    // it, so nothing is invented.
    let id_less = vec![
        json!({"role": "assistant", "content": [{"functionCall": {"name": "run", "args": {}}}]}),
        json!({"role": "user", "content": "next"}),
    ];
    assert!(answer_unanswered_tool_calls(&id_less).is_none());
}

fn payload(messages: Vec<Value>, provider: &str, model: &str) -> LlmRequestPayload {
    LlmRequestPayload::from(&LlmCallOptions {
        provider: provider.to_string(),
        model: model.to_string(),
        messages,
        max_tokens: 64,
        ..LlmCallOptions::default()
    })
}

/// OpenAI Responses refuses a `function_call` with no `function_call_output`
/// of the same `call_id`: "No tool output found for function call".
fn responses_unanswered(body: &Value) -> Vec<String> {
    let items = body["input"].as_array().cloned().unwrap_or_default();
    let answered: Vec<&Value> = items
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| &item["call_id"])
        .collect();
    items
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter(|item| !answered.contains(&&item["call_id"]))
        .map(|item| item["call_id"].to_string())
        .collect()
}

/// Chat Completions refuses an assistant `tool_calls` id with no `role: tool`
/// message carrying it.
fn chat_unanswered(body: &Value) -> Vec<String> {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let answered: Vec<&Value> = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| &message["tool_call_id"])
        .collect();
    messages
        .iter()
        .flat_map(|message| {
            message["tool_calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|call| !answered.contains(&&call["id"]))
        .map(|call| call["id"].to_string())
        .collect()
}

/// Anthropic refuses a `tool_use` whose id has no `tool_result` in the very
/// next message.
fn anthropic_unanswered(body: &Value) -> Vec<String> {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let mut open = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let next = messages.get(index + 1);
        for block in message["content"].as_array().cloned().unwrap_or_default() {
            if block["type"] != "tool_use" {
                continue;
            }
            let answered = next
                .and_then(|next| next["content"].as_array())
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == block["id"])
                });
            if !answered {
                open.push(block["id"].to_string());
            }
        }
    }
    open
}

#[test]
fn every_provider_request_after_a_stopped_tool_call_is_well_formed() {
    let responses = payload(stopped_mid_tool(), "openai", "gpt-5");
    assert_eq!(
        responses_unanswered(&OpenAiResponsesProvider::build_request_body(&responses)),
        Vec::<String>::new()
    );
    let chat = payload(stopped_mid_tool(), "openai", "gpt-5");
    assert_eq!(
        chat_unanswered(&OpenAiCompatibleProvider::build_request_body(&chat)),
        Vec::<String>::new()
    );
    let anthropic = payload(stopped_mid_tool(), "anthropic", "claude-sonnet-4-6");
    assert_eq!(
        anthropic_unanswered(&AnthropicProvider::build_request_body(&anthropic)),
        Vec::<String>::new()
    );
}

/// Negative control: the same history with the repair bypassed is exactly the
/// request the providers refused, so the checks above are not vacuous.
#[test]
fn the_unrepaired_history_is_the_request_providers_refuse() {
    let mut raw = payload(Vec::new(), "openai", "gpt-5");
    raw.messages = stopped_mid_tool();
    assert_eq!(
        responses_unanswered(&OpenAiResponsesProvider::build_request_body(&raw)),
        vec!["\"call_stopped\"".to_string()]
    );
    assert_eq!(
        chat_unanswered(&OpenAiCompatibleProvider::build_request_body(&raw)),
        vec!["\"call_stopped\"".to_string()]
    );
}
