//! Stream/non-stream parity for every dialect whose stream is decoded by the
//! SSE transport.
//!
//! Each case is one complete provider response written as the non-streaming
//! body. The same response is replayed as the provider's stream frames, built
//! by the documented frame grammar below, and both decodings must produce the
//! same `LlmResult`. A field that differs means a feature was implemented on
//! one path and not mirrored on the other.

use super::sse::consume_sse_lines;
use crate::llm::api::{DialectContract, LlmCallOptions, LlmRequestPayload, LlmResult};
use crate::value::VmError;
use serde_json::{json, Value};

/// Split a string into a few uneven pieces, the way providers chunk deltas.
fn pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return vec![text.to_string()];
    }
    let cut_a = chars.len() / 3;
    let cut_b = (chars.len() * 2) / 3 + 1;
    vec![
        chars[..cut_a].iter().collect(),
        chars[cut_a..cut_b].iter().collect(),
        chars[cut_b..].iter().collect(),
    ]
}

fn sse(events: &[(Option<&str>, Value)]) -> String {
    let mut out = String::new();
    for (name, data) in events {
        if let Some(name) = name {
            out.push_str(&format!("event: {name}\n"));
        }
        out.push_str(&format!("data: {data}\n\n"));
    }
    out
}

/// Anthropic Messages streaming grammar for a complete message body.
fn anthropic_frames(body: &Value) -> String {
    let mut message = body.clone();
    let content = message
        .as_object_mut()
        .and_then(|object| object.remove("content"))
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let usage = body.get("usage").cloned().unwrap_or_else(|| json!({}));
    let mut start_usage = usage.clone();
    if let Some(object) = start_usage.as_object_mut() {
        object.insert("output_tokens".into(), json!(1));
    }
    message["content"] = json!([]);
    message["stop_reason"] = Value::Null;
    message["usage"] = start_usage;
    let mut events = vec![(
        Some("message_start"),
        json!({"type": "message_start", "message": message}),
    )];
    for (index, block) in content.iter().enumerate() {
        let block_type = block["type"].as_str().unwrap_or_default();
        let mut start = block.clone();
        let mut deltas = Vec::new();
        match block_type {
            "text" => {
                start["text"] = json!("");
                for piece in pieces(block["text"].as_str().unwrap_or_default()) {
                    deltas.push(json!({"type": "text_delta", "text": piece}));
                }
            }
            "thinking" => {
                start["thinking"] = json!("");
                start["signature"] = json!("");
                for piece in pieces(block["thinking"].as_str().unwrap_or_default()) {
                    deltas.push(json!({"type": "thinking_delta", "thinking": piece}));
                }
                if let Some(signature) = block["signature"].as_str() {
                    deltas.push(json!({"type": "signature_delta", "signature": signature}));
                }
            }
            "tool_use" | "server_tool_use" => {
                start["input"] = json!({});
                let input = &block["input"];
                if input.as_object().is_some_and(|object| !object.is_empty()) {
                    for piece in pieces(&input.to_string()) {
                        deltas.push(json!({"type": "input_json_delta", "partial_json": piece}));
                    }
                }
            }
            _ => {}
        }
        events.push((
            Some("content_block_start"),
            json!({"type": "content_block_start", "index": index, "content_block": start}),
        ));
        for delta in deltas {
            events.push((
                Some("content_block_delta"),
                json!({"type": "content_block_delta", "index": index, "delta": delta}),
            ));
        }
        events.push((
            Some("content_block_stop"),
            json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push((
        Some("message_delta"),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": body["stop_reason"], "stop_sequence": null},
            "usage": {"output_tokens": usage["output_tokens"]},
        }),
    ));
    events.push((Some("message_stop"), json!({"type": "message_stop"})));
    sse(&events)
}

/// OpenAI Chat Completions streaming grammar for a complete body: a role
/// chunk, chunked content and reasoning deltas, per-index tool-call deltas
/// whose first chunk names the call, then a finish chunk carrying usage.
fn openai_frames(body: &Value) -> String {
    let choice = &body["choices"][0];
    let message = &choice["message"];
    let envelope = |delta: Value, extra: Value| {
        let mut chunk = json!({
            "id": body["id"],
            "object": "chat.completion.chunk",
            "model": body["model"],
            "choices": [{"index": 0, "delta": delta, "finish_reason": null}],
        });
        if let Some(object) = extra.as_object() {
            for (key, value) in object {
                chunk[key] = value.clone();
            }
        }
        chunk
    };
    let mut chunks = vec![envelope(
        json!({"role": "assistant", "content": ""}),
        json!({}),
    )];
    for field in ["reasoning_content", "reasoning"] {
        if let Some(reasoning) = message[field].as_str() {
            for piece in pieces(reasoning) {
                chunks.push(envelope(json!({ field: piece }), json!({})));
            }
        }
    }
    if let Some(content) = message["content"].as_str() {
        for (index, piece) in pieces(content).into_iter().enumerate() {
            let mut chunk = envelope(json!({"content": piece}), json!({}));
            if index == 0 {
                if let Some(logprobs) = choice.get("logprobs") {
                    chunk["choices"][0]["logprobs"] = logprobs.clone();
                }
            }
            chunks.push(chunk);
        }
    }
    for (index, call) in message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if call.get("function").is_none() {
            let mut call = call.clone();
            call["index"] = json!(index);
            chunks.push(envelope(json!({"tool_calls": [call]}), json!({})));
            continue;
        }
        let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
        let mut head = json!({
            "index": index,
            "type": "function",
            "function": {"name": call["function"]["name"], "arguments": ""},
        });
        if let Some(id) = call.get("id") {
            head["id"] = id.clone();
        }
        chunks.push(envelope(json!({"tool_calls": [head]}), json!({})));
        for piece in pieces(arguments) {
            chunks.push(envelope(
                json!({"tool_calls": [{"index": index, "function": {"arguments": piece}}]}),
                json!({}),
            ));
        }
    }
    let mut finish = envelope(json!({}), json!({}));
    finish["choices"][0]["finish_reason"] = choice["finish_reason"].clone();
    for key in ["usage", "service_tier", "system_fingerprint"] {
        if let Some(value) = body.get(key) {
            finish[key] = value.clone();
        }
    }
    chunks.push(finish);
    let mut out = String::new();
    for chunk in chunks {
        out.push_str(&format!("data: {chunk}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

struct Case {
    name: &'static str,
    provider: &'static str,
    model: &'static str,
    tools_offered: bool,
    /// The provider sent no tool-call id. A stream must name the call before
    /// the response completes, so it synthesizes `stream-tool-N-<uuid>` where
    /// the body parser synthesizes `call_N_M`; only the id spelling differs.
    synthesized_ids: bool,
    body: Value,
}

fn anthropic(name: &'static str, model: &'static str, body: Value) -> Case {
    Case {
        name,
        provider: "anthropic",
        model,
        tools_offered: true,
        synthesized_ids: false,
        body,
    }
}

fn openai(name: &'static str, provider: &'static str, model: &'static str, body: Value) -> Case {
    Case {
        name,
        provider,
        model,
        tools_offered: true,
        synthesized_ids: false,
        body,
    }
}

fn chat(message: Value, finish_reason: &str, usage: Value) -> Value {
    json!({
        "id": "chatcmpl-parity",
        "object": "chat.completion",
        "model": "parity-model",
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
        "usage": usage,
    })
}

fn messages(content: Value, stop_reason: &str, usage: Value) -> Value {
    json!({
        "id": "msg_parity",
        "type": "message",
        "role": "assistant",
        "model": "claude-parity",
        "content": content,
        "stop_reason": stop_reason,
        "usage": usage,
    })
}

fn cases() -> Vec<Case> {
    let usage = json!({"input_tokens": 12, "output_tokens": 7});
    let cached_usage = json!({
        "input_tokens": 12,
        "output_tokens": 7,
        "cache_read_input_tokens": 40,
        "cache_creation_input_tokens": 5,
    });
    let oai_usage = json!({"prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19});
    vec![
        anthropic(
            "anthropic text with cache usage",
            "claude-sonnet-4-6",
            messages(
                json!([{"type": "text", "text": "The answer is 42."}]),
                "end_turn",
                cached_usage,
            ),
        ),
        anthropic(
            "anthropic two text blocks",
            "claude-sonnet-4-6",
            messages(
                json!([
                    {"type": "text", "text": "First paragraph."},
                    {"type": "text", "text": "Second paragraph."}
                ]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic signed thinking",
            "claude-sonnet-4-6",
            messages(
                json!([
                    {"type": "thinking", "thinking": "Weigh both options.", "signature": "sig-abc"},
                    {"type": "text", "text": "Option A."}
                ]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic unsigned thinking",
            "claude-sonnet-4-6",
            messages(
                json!([
                    {"type": "thinking", "thinking": "Unsigned trace."},
                    {"type": "text", "text": "Answer."}
                ]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic thinking-only turn with tools offered",
            "claude-sonnet-4-6",
            messages(
                json!([{"type": "thinking", "thinking": "I should call a tool.", "signature": "sig-x"}]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic redacted thinking",
            "claude-sonnet-4-6",
            messages(
                json!([
                    {"type": "redacted_thinking", "data": "opaque-redacted"},
                    {"type": "text", "text": "Done."}
                ]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic tool use with toolset",
            "claude-sonnet-4-6",
            messages(
                json!([
                    {"type": "text", "text": "Reading."},
                    {"type": "tool_use", "id": "toolu_1", "name": "screenshot", "toolset_name": "computer", "input": {"scale": 2}},
                    {"type": "tool_use", "id": "toolu_2", "name": "read", "input": {"path": "a.rs"}}
                ]),
                "tool_use",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic tool use without arguments",
            "claude-sonnet-4-6",
            messages(
                json!([{"type": "tool_use", "id": "toolu_3", "name": "list", "input": {}}]),
                "tool_use",
                usage.clone(),
            ),
        ),
        anthropic(
            "anthropic server tool search",
            "claude-opus-4-7",
            messages(
                json!([
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "tool_search_tool_bm25", "input": {"query": "weather"}},
                    {"type": "tool_search_tool_result", "tool_use_id": "srvtoolu_1", "content": {"type": "tool_search_tool_search_result", "tool_references": [{"type": "tool_reference", "tool_name": "get_weather"}]}},
                    {"type": "text", "text": "Found it."}
                ]),
                "end_turn",
                usage.clone(),
            ),
        ),
        anthropic("anthropic served fast", "claude-opus-4-8", {
            let mut body = messages(
                json!([{"type": "text", "text": "Fast."}]),
                "end_turn",
                usage.clone(),
            );
            body["usage"]["speed"] = json!("fast");
            body
        }),
        anthropic(
            "anthropic billed empty turn",
            "claude-sonnet-4-6",
            messages(json!([]), "end_turn", usage),
        ),
        openai(
            "openai text with cached usage",
            "openai",
            "gpt-5.4",
            chat(
                json!({"role": "assistant", "content": "The answer is 42."}),
                "stop",
                json!({"prompt_tokens": 12, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 8}}),
            ),
        ),
        openai(
            "openai reasoning content",
            "openrouter",
            "qwen/qwen3.6-35b-a3b",
            chat(
                json!({"role": "assistant", "content": "Answer.", "reasoning_content": "Think it through step by step."}),
                "stop",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai inline think block",
            "openrouter",
            "qwen/qwen3.6-35b-a3b",
            chat(
                json!({"role": "assistant", "content": "<think>private plan</think>Visible answer."}),
                "stop",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai parallel tool calls",
            "openai",
            "gpt-5.4",
            chat(
                json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_a", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}},
                    {"id": "call_b", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"b.rs\"}"}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        Case {
            synthesized_ids: true,
            ..openai(
                "openai tool call without provider id",
                "openrouter",
                "google/gemma-4",
                chat(
                    json!({"role": "assistant", "content": "", "tool_calls": [
                        {"type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}}
                    ]}),
                    "tool_calls",
                    oai_usage.clone(),
                ),
            )
        },
        openai(
            "openai tool call without arguments",
            "openai",
            "gpt-5.4",
            chat(
                json!({"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_none", "type": "function", "function": {"name": "list", "arguments": ""}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        openai("openai logprobs", "openai", "gpt-5.4", {
            let mut body = chat(
                json!({"role": "assistant", "content": "safe"}),
                "stop",
                oai_usage.clone(),
            );
            body["choices"][0]["logprobs"] = json!({"content": [
                {"token": "safe", "logprob": -0.1, "bytes": [115, 97, 102, 101], "top_logprobs": []}
            ]});
            body
        }),
        openai(
            "openai text call in tool name",
            "zai",
            "glm-5",
            chat(
                json!({"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_name", "type": "function", "function": {"name": "look({ file: \"a.rs\", intent: \"read\" })", "arguments": "{}"}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai text call in generic wrapper arguments",
            "openrouter",
            "gpt-oss-120b",
            chat(
                json!({"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_wrap", "type": "function", "function": {"name": "tool_call", "arguments": "<tool_call>\nlook({ file: \"a.rs\", intent: \"read\" })\n</tool_call>"}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai concatenated argument objects",
            "openrouter",
            "google/gemma-4",
            chat(
                json!({"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_cat", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}{\"path\":\"b.rs\"}"}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai length-truncated tool arguments",
            "openrouter",
            "or-qwen",
            chat(
                json!({"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_cut", "type": "function", "function": {"name": "edit", "arguments": "{\"path\":\"a.rs\",\"content\":\"unterminated"}}
                ]}),
                "length",
                oai_usage.clone(),
            ),
        ),
        openai(
            "openai server tool search",
            "openai",
            "gpt-5.4",
            chat(
                json!({"role": "assistant", "content": "searching", "tool_calls": [
                    {"id": "tsc_01", "type": "tool_search_call", "query": {"q": "weather"}}
                ]}),
                "tool_calls",
                oai_usage.clone(),
            ),
        ),
        openai("openai served fast", "openai", "gpt-5.5", {
            let mut body = chat(
                json!({"role": "assistant", "content": "Fast."}),
                "stop",
                oai_usage.clone(),
            );
            body["service_tier"] = json!("priority");
            body
        }),
        openai(
            "openai billed hidden reasoning with tools offered",
            "openrouter",
            "qwen/qwen3.6-35b-a3b",
            chat(
                json!({"role": "assistant", "content": "", "reasoning_content": "creating files"}),
                "stop",
                oai_usage,
            ),
        ),
    ]
}

/// The comparable projection of a decode. Errors compare by message, since
/// that is what a caller sees; volatile client timing is dropped.
fn projection(result: Result<LlmResult, VmError>, synthesized_ids: bool) -> Value {
    match result {
        Ok(result) => {
            let mut value = serde_json::to_value(&result).expect("result serializes");
            if let Some(telemetry) = value.get_mut("telemetry").and_then(Value::as_object_mut) {
                telemetry.remove("client_first_frame_ms");
            }
            if synthesized_ids {
                mask_synthesized_ids(&mut value);
            }
            value
        }
        Err(error) => json!({"error": error.to_string()}),
    }
}

/// Replace either side's synthesized tool-call id, and drop the id the stream
/// writes into its provider receipt, so the rest of the call still compares.
fn mask_synthesized_ids(value: &mut Value) {
    match value {
        Value::String(text)
            if text.starts_with("stream-tool-")
                || text
                    .strip_prefix("call_")
                    .is_some_and(|rest| rest.contains('_')) =>
        {
            *text = "<synthesized>".to_string();
        }
        Value::Array(items) => items.iter_mut().for_each(mask_synthesized_ids),
        Value::Object(object) => {
            if let Some(raw) = object
                .get_mut("raw_tool_calls")
                .and_then(Value::as_array_mut)
            {
                for call in raw.iter_mut().filter_map(Value::as_object_mut) {
                    call.remove("id");
                }
            }
            object.values_mut().for_each(mask_synthesized_ids);
        }
        _ => {}
    }
}

/// Field names at which two projections differ, recursing through objects.
fn differing_fields(prefix: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                differing_fields(
                    &path,
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if left != right => out.push(prefix.to_string()),
        _ => {}
    }
}

async fn decode_both(case: &Case) -> (Value, Value) {
    let is_anthropic = case.provider == "anthropic";
    let wire = if is_anthropic {
        crate::llm::capabilities::WireDialect::Anthropic
    } else {
        crate::llm::capabilities::WireDialect::OpenAiCompat
    };
    let body = crate::llm::api::parse_llm_response_for_provider(
        &case.body,
        case.provider,
        case.model,
        wire,
        case.tools_offered,
    );
    let frames = if is_anthropic {
        anthropic_frames(&case.body)
    } else {
        openai_frames(&case.body)
    };
    let request = LlmRequestPayload::from(&LlmCallOptions {
        provider: case.provider.to_string(),
        model: case.model.to_string(),
        ..LlmCallOptions::default()
    });
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let streamed = consume_sse_lines(
        tokio::io::BufReader::new(frames.as_bytes()),
        case.provider,
        case.model,
        DialectContract::for_request(&request),
        delta_tx,
        None,
        None,
        case.tools_offered,
    )
    .await;
    (
        projection(body, case.synthesized_ids),
        projection(streamed, case.synthesized_ids),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn every_streamed_response_decodes_like_its_body() {
    let mut divergent = Vec::new();
    for case in cases() {
        let (body, streamed) = decode_both(&case).await;
        let mut fields = Vec::new();
        differing_fields("", &body, &streamed, &mut fields);
        if !fields.is_empty() {
            divergent.push(format!(
                "{}: {}\n  body:   {body}\n  stream: {streamed}",
                case.name,
                fields.join(", ")
            ));
        }
    }
    assert!(
        divergent.is_empty(),
        "{} of {} responses decode differently when streamed:\n{}",
        divergent.len(),
        cases().len(),
        divergent.join("\n")
    );
}
