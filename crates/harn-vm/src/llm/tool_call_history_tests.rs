//! The tool-call history repair, pinned at the shape level and at every
//! provider dialect's real request body. A stopped turn (harn#8951), runtime
//! feedback injected before a result, and a result whose call was compacted
//! away each produced a request some provider refused.

use super::{
    repair_tool_call_history, UNANSWERED_TOOL_CALL_OBSERVATION, UNPAIRED_TOOL_RESULT_MARKER,
};
use crate::llm::api::{LlmCallOptions, LlmRequestPayload};
use crate::llm::providers::{
    AnthropicProvider, BedrockProvider, GeminiInteractions, GeminiProvider, OllamaProvider,
    OpenAiCompatibleProvider, OpenAiResponsesProvider,
};
use serde_json::{json, Value};

fn openai_call(id: &str) -> Value {
    json!({"id": id, "type": "function", "function": {"name": "run", "arguments": "{}"}})
}

fn call_turn(ids: &[&str]) -> Value {
    let calls: Vec<Value> = ids.iter().map(|id| openai_call(id)).collect();
    json!({"role": "assistant", "content": "", "tool_calls": calls})
}

fn tool_result(id: &str) -> Value {
    json!({"role": "tool_result", "name": "run", "tool_call_id": id, "content": "ok"})
}

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

/// The harn#8951 transcript: the call was cancelled, then the person typed again.
fn stopped_mid_tool() -> Vec<Value> {
    vec![
        user("run the slow command"),
        call_turn(&["call_stopped"]),
        user("try again"),
    ]
}

/// Compaction removed the call but kept its result.
fn orphaned_result() -> Vec<Value> {
    vec![
        user("run the slow command"),
        json!({"role": "assistant", "content": "Running it."}),
        tool_result("call_compacted"),
        user("what happened?"),
    ]
}

/// Runtime feedback was injected between a call and its result.
fn interleaved_feedback() -> Vec<Value> {
    vec![
        user("run the slow command"),
        call_turn(&["call_slow"]),
        user("<runtime_feedback>budget is low</runtime_feedback>"),
        tool_result("call_slow"),
        user("continue"),
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
    let repaired = repair_tool_call_history(stopped_mid_tool());
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
    let messages = vec![call_turn(&["a", "b"]), tool_result("a"), user("next")];
    let repaired = repair_tool_call_history(messages);
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
        user("next"),
    ];
    let repaired = repair_tool_call_history(messages);
    assert!(is_repair_for(&repaired[1], "toolu_1"), "{}", repaired[1]);
}

#[test]
fn interleaved_feedback_moves_after_the_result_it_preceded() {
    let repaired = repair_tool_call_history(interleaved_feedback());
    let roles: Vec<&str> = repaired
        .iter()
        .map(|message| message["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool_result", "user", "user"]);
    assert_eq!(repaired[2]["tool_call_id"], "call_slow");
    assert_eq!(
        repaired[3]["content"],
        "<runtime_feedback>budget is low</runtime_feedback>"
    );
}

#[test]
fn an_orphaned_result_is_kept_as_an_observation() {
    let repaired = repair_tool_call_history(orphaned_result());
    assert_eq!(repaired.len(), 4);
    assert_eq!(repaired[2]["role"], "user");
    assert_eq!(
        repaired[2]["content"],
        format!("{UNPAIRED_TOOL_RESULT_MARKER}\nok")
    );
}

#[test]
fn an_orphaned_result_block_becomes_text_beside_the_paired_ones() {
    let messages = vec![
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_valid", "name": "run", "input": {}}
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_valid", "content": "valid"},
            {"type": "tool_result", "tool_use_id": "", "content": "legacy observation"},
        ]}),
    ];
    let repaired = repair_tool_call_history(messages);
    assert_eq!(repaired.len(), 2);
    assert_eq!(repaired[1]["content"][0]["type"], "tool_result");
    assert_eq!(
        repaired[1]["content"][1],
        json!({"type": "text", "text": format!("{UNPAIRED_TOOL_RESULT_MARKER}\nlegacy observation")})
    );
}

#[test]
fn a_stray_result_does_not_hide_the_real_one_behind_it() {
    let messages = vec![
        call_turn(&["real"]),
        tool_result("stray"),
        tool_result("real"),
    ];
    let repaired = repair_tool_call_history(messages);
    assert_eq!(repaired[1]["tool_call_id"], "real");
    assert_eq!(repaired[2]["role"], "user");
    assert_eq!(repaired.len(), 3);
}

#[test]
fn a_well_formed_history_and_an_id_less_call_are_left_alone() {
    let answered = vec![call_turn(&["a"]), tool_result("a")];
    assert_eq!(repair_tool_call_history(answered.clone()), answered);
    // Anthropic answers inside a user turn's content blocks, possibly across
    // several consecutive user messages.
    let block_answered = vec![
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {}},
            {"type": "tool_use", "id": "toolu_02", "name": "read_file", "input": {}}
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_01", "content": "one"}
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_02", "content": "two"}
        ]}),
    ];
    assert_eq!(
        repair_tool_call_history(block_answered.clone()),
        block_answered
    );
    // Gemini's `functionCall` id is optional; no provider can pair a result to
    // it, so nothing is invented.
    let id_less = vec![
        json!({"role": "assistant", "content": [{"functionCall": {"name": "run", "args": {}}}]}),
        user("next"),
    ];
    assert_eq!(repair_tool_call_history(id_less.clone()), id_less);
}

// ---- Every dialect's real request body ------------------------------------

/// One provider turn reduced to the call ids it issues and the call ids it
/// answers, so a single pairing rule checks every wire grammar.
#[derive(Default)]
struct WireTurn {
    calls: Vec<String>,
    results: Vec<String>,
}

fn ids(values: impl Iterator<Item = Option<String>>) -> Vec<String> {
    values.flatten().collect()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

fn blocks(message: &Value, key: &str) -> Vec<Value> {
    message[key].as_array().cloned().unwrap_or_default()
}

/// Chat Completions and Ollama: assistant `tool_calls`, then `role: tool`.
fn chat_turns(body: &Value) -> Vec<WireTurn> {
    blocks(body, "messages")
        .iter()
        .map(|message| WireTurn {
            calls: ids(blocks(message, "tool_calls")
                .iter()
                .map(|call| text(&call["id"]))),
            results: if message["role"] == "tool" {
                vec![text(&message["tool_call_id"]).unwrap_or_default()]
            } else {
                Vec::new()
            },
        })
        .collect()
}

/// Anthropic: `tool_use` blocks, then `tool_result` blocks.
fn anthropic_turns(body: &Value) -> Vec<WireTurn> {
    blocks(body, "messages")
        .iter()
        .map(|message| {
            let content = blocks(message, "content");
            WireTurn {
                calls: ids(content
                    .iter()
                    .filter(|block| block["type"] == "tool_use")
                    .map(|block| text(&block["id"]))),
                results: ids(content
                    .iter()
                    .filter(|block| block["type"] == "tool_result")
                    .map(|block| Some(text(&block["tool_use_id"]).unwrap_or_default()))),
            }
        })
        .collect()
}

/// Responses: `function_call` items, then `function_call_output` items.
fn responses_turns(body: &Value) -> Vec<WireTurn> {
    let mut turns: Vec<WireTurn> = Vec::new();
    for item in blocks(body, "input") {
        let call = (item["type"] == "function_call").then(|| text(&item["call_id"]));
        let result = (item["type"] == "function_call_output")
            .then(|| text(&item["call_id"]).unwrap_or_default());
        // Parallel calls are consecutive items of one assistant turn.
        if let (Some(id), Some(last)) = (call.clone().flatten(), turns.last_mut()) {
            if !last.calls.is_empty() && last.results.is_empty() {
                last.calls.push(id);
                continue;
            }
        }
        turns.push(WireTurn {
            calls: call.flatten().into_iter().collect(),
            results: result.into_iter().collect(),
        });
    }
    turns
}

/// Gemini generateContent: `functionCall` parts, then `functionResponse` parts.
fn gemini_turns(body: &Value) -> Vec<WireTurn> {
    blocks(body, "contents")
        .iter()
        .map(|content| {
            let parts = blocks(content, "parts");
            WireTurn {
                calls: ids(parts.iter().map(|part| text(&part["functionCall"]["id"]))),
                results: ids(parts.iter().map(|part| {
                    part.get("functionResponse")
                        .map(|response| text(&response["id"]).unwrap_or_default())
                })),
            }
        })
        .collect()
}

/// Gemini Interactions: `function_call` steps, then `function_result` steps.
fn interactions_turns(body: &Value) -> Vec<WireTurn> {
    blocks(body, "input")
        .iter()
        .map(|step| WireTurn {
            calls: ids(std::iter::once(
                (step["type"] == "function_call")
                    .then(|| text(&step["id"]))
                    .flatten(),
            )),
            results: if step["type"] == "function_result" {
                vec![text(&step["call_id"]).unwrap_or_default()]
            } else {
                Vec::new()
            },
        })
        .fold(Vec::new(), |mut turns: Vec<WireTurn>, turn| {
            match turns.last_mut() {
                Some(last)
                    if !turn.calls.is_empty()
                        && !last.calls.is_empty()
                        && last.results.is_empty() =>
                {
                    last.calls.extend(turn.calls);
                }
                _ => turns.push(turn),
            }
            turns
        })
}

/// Bedrock Converse: `toolUse` blocks, then `toolResult` blocks.
fn bedrock_turns(body: &Value) -> Vec<WireTurn> {
    blocks(body, "messages")
        .iter()
        .map(|message| {
            let content = blocks(message, "content");
            WireTurn {
                calls: ids(content
                    .iter()
                    .map(|block| text(&block["toolUse"]["toolUseId"]))),
                results: ids(content.iter().map(|block| {
                    block
                        .get("toolResult")
                        .map(|result| text(&result["toolUseId"]).unwrap_or_default())
                })),
            }
        })
        .collect()
}

/// The pairing rule every native tool-calling API enforces: each call is
/// answered by the result-carrying turns that directly follow it, and every
/// result answers a still-open call of the assistant turn just before it.
fn pairing_violations(turns: &[WireTurn]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut open: Vec<String> = Vec::new();
    for turn in turns {
        if turn.results.is_empty() {
            violations.extend(open.drain(..).map(|id| format!("unanswered {id}")));
        }
        for id in &turn.results {
            match open.iter().position(|candidate| candidate == id) {
                Some(index) => {
                    open.remove(index);
                }
                None => violations.push(format!("orphan {id}")),
            }
        }
        if !turn.calls.is_empty() {
            violations.extend(open.drain(..).map(|id| format!("unanswered {id}")));
            open = turn.calls.clone();
        }
    }
    violations.extend(open.into_iter().map(|id| format!("unanswered {id}")));
    violations
}

struct Dialect {
    name: &'static str,
    provider: &'static str,
    model: &'static str,
    build: fn(&LlmRequestPayload) -> Value,
    turns: fn(&Value) -> Vec<WireTurn>,
}

fn dialects() -> [Dialect; 7] {
    [
        Dialect {
            name: "anthropic",
            provider: "anthropic",
            model: "claude-sonnet-4-6",
            build: AnthropicProvider::build_request_body,
            turns: anthropic_turns,
        },
        Dialect {
            name: "openai_chat",
            provider: "openai",
            model: "gpt-5",
            build: OpenAiCompatibleProvider::build_request_body,
            turns: chat_turns,
        },
        Dialect {
            name: "openai_responses",
            provider: "openai",
            model: "gpt-5",
            build: OpenAiResponsesProvider::build_request_body,
            turns: responses_turns,
        },
        Dialect {
            name: "gemini",
            provider: "gemini",
            model: "gemini-3.5-flash-lite",
            build: GeminiProvider::build_request_body,
            turns: gemini_turns,
        },
        Dialect {
            name: "gemini_interactions",
            provider: "gemini",
            model: "gemini-3.5-flash-lite",
            build: GeminiInteractions::build_request_body,
            turns: interactions_turns,
        },
        Dialect {
            name: "bedrock",
            provider: "bedrock",
            model: "anthropic.claude-sonnet-4-6",
            build: BedrockProvider::build_request_body,
            turns: bedrock_turns,
        },
        Dialect {
            name: "ollama",
            provider: "ollama",
            model: "llama3.2",
            build: OllamaProvider::build_request_body,
            turns: chat_turns,
        },
    ]
}

fn scenarios() -> [(&'static str, Vec<Value>); 3] {
    [
        ("stopped mid-tool", stopped_mid_tool()),
        ("orphaned result", orphaned_result()),
        ("interleaved feedback", interleaved_feedback()),
    ]
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

#[test]
fn every_dialect_sends_a_well_paired_history_for_every_broken_one() {
    for dialect in dialects() {
        for (scenario, messages) in scenarios() {
            let body = (dialect.build)(&payload(messages, dialect.provider, dialect.model));
            let violations = pairing_violations(&(dialect.turns)(&body));
            assert!(
                violations.is_empty(),
                "{} / {scenario}: {violations:?}\n{body:#}",
                dialect.name
            );
        }
    }
}

/// Negative control: the same histories with the repair bypassed are exactly
/// the requests providers refuse, so the check above is not vacuous. The one
/// exception is Bedrock's interleaved feedback: Converse folds consecutive
/// user turns into one, so the feedback and the result already share a turn.
#[test]
fn every_unrepaired_history_breaks_pairing_on_every_dialect() {
    let mut paired_without_repair = Vec::new();
    for dialect in dialects() {
        for (scenario, messages) in scenarios() {
            let mut raw = payload(Vec::new(), dialect.provider, dialect.model);
            raw.messages = messages;
            let violations = pairing_violations(&(dialect.turns)(&(dialect.build)(&raw)));
            if violations.is_empty() {
                paired_without_repair.push(format!("{} / {scenario}", dialect.name));
            }
        }
    }
    assert_eq!(paired_without_repair, ["bedrock / interleaved feedback"]);
}

/// A provider-shaped history still yields each result as the model saw it,
/// named by the call it answers, so a host never parses the block dialect.
#[test]
fn seen_tool_results_reads_anthropic_result_blocks() {
    let messages = vec![
        user("read it"),
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}},
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                {"type": "text", "text": "fn main() {}"},
                {"type": "text", "text": "// end"},
            ]},
        ]}),
    ];
    let seen = super::seen_tool_results(&messages);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].message, 2);
    assert_eq!(seen[0].tool_call_id.as_deref(), Some("toolu_1"));
    assert_eq!(seen[0].tool.as_deref(), Some("read_file"));
    assert_eq!(seen[0].text, "fn main() {}\n// end");
}
