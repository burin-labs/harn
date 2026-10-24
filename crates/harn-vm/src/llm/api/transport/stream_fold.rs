//! Folds a provider's stream frames into the provider's own complete response.
//!
//! A streamed turn and an unstreamed turn describe the same response, so they
//! are decoded by the same parser: the stream is folded back into the body the
//! provider would have returned without streaming, and
//! [`crate::llm::api::response`] reads that body. Gemini (`merge_response`),
//! Gemini Interactions (`InteractionStream`), and OpenAI Responses
//! (`response.completed`) already work this way; this module does it for the
//! two dialects whose streams the SSE transport reads, Anthropic Messages and
//! OpenAI Chat Completions.
//!
//! The fold also reports what each frame added as [`StreamPart`]s. The
//! transport uses them for everything that must happen while the stream is
//! still open: forwarding text, the structured-output abort, and tool-call
//! progress events. Nothing in the final result is derived from them.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::super::openai_normalize::{extract_openai_delta_field_str, preview_chars};
use super::StreamProtocol;

/// What one frame added that the transport must act on before the stream ends.
#[derive(Debug, PartialEq)]
pub(super) enum StreamPart {
    /// Raw visible-text delta. OpenAI-compatible content may still contain
    /// inline `<think>` spans; the transport strips them before forwarding.
    Text(String),
    /// Reasoning-channel delta. Never forwarded as text; only metered.
    Reasoning(String),
    /// A dispatchable tool call started or its arguments grew. The value
    /// indexes [`StreamFold::tool`].
    ToolCall(usize),
}

/// A dispatchable tool call as the stream has delivered it so far.
#[derive(Debug)]
pub(super) struct StreamToolCall {
    /// The id progress events use and the call dispatches under: the
    /// provider's id, or a synthesized one when the provider sent none.
    pub(super) tool_call_id: String,
    pub(super) name: String,
    pub(super) arguments: String,
    /// Set once a progress event used `tool_call_id`, which then never
    /// changes even if the provider's own id arrives later.
    published: bool,
}

impl StreamToolCall {
    fn new(provider_id: &str, name: &str, ordinal: usize) -> Self {
        Self {
            tool_call_id: streaming_tool_call_id(provider_id, ordinal),
            name: name.to_string(),
            arguments: String::new(),
            published: false,
        }
    }
}

/// Canonical wire id for a streamed tool call.
///
/// Progress events and the executed lifecycle join on `tool_call_id`, so the
/// provider id is used verbatim whenever present. A synthesized fallback
/// carries a UUID because it becomes the dispatch id; a bare per-stream index
/// would collide across loop iterations.
fn streaming_tool_call_id(provider_id: &str, ordinal: usize) -> String {
    if provider_id.is_empty() {
        format!("stream-tool-{ordinal}-{}", uuid::Uuid::now_v7())
    } else {
        provider_id.to_string()
    }
}

pub(super) enum StreamFold {
    Anthropic(AnthropicFold),
    OpenAi(OpenAiFold),
}

impl StreamFold {
    pub(super) fn new(protocol: StreamProtocol) -> Self {
        if protocol == StreamProtocol::AnthropicSse {
            Self::Anthropic(AnthropicFold::default())
        } else {
            Self::OpenAi(OpenAiFold::default())
        }
    }

    pub(super) fn observe(&mut self, frame: &Value) -> Vec<StreamPart> {
        match self {
            Self::Anthropic(fold) => fold.observe(frame),
            Self::OpenAi(fold) => fold.observe(frame),
        }
    }

    pub(super) fn tool(&self, slot: usize) -> &StreamToolCall {
        match self {
            Self::Anthropic(fold) => &fold.tools[slot],
            Self::OpenAi(fold) => &fold.tools[slot],
        }
    }

    /// Freeze the call's id: a progress event has used it.
    pub(super) fn publish(&mut self, slot: usize) {
        match self {
            Self::Anthropic(fold) => fold.tools[slot].published = true,
            Self::OpenAi(fold) => fold.tools[slot].published = true,
        }
    }

    /// The complete response body this stream described.
    pub(super) fn into_body(self) -> Value {
        match self {
            Self::Anthropic(fold) => fold.into_body(),
            Self::OpenAi(fold) => fold.into_body(),
        }
    }
}

/// Merge `source`'s non-null fields into `target`, later frames winning.
fn merge_fields(target: &mut Map<String, Value>, source: &Value, skip: &[&str]) {
    let Some(source) = source.as_object() else {
        return;
    };
    for (key, value) in source {
        if !value.is_null() && !skip.contains(&key.as_str()) {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// One Anthropic content block under construction.
struct AnthropicBlock {
    block: Value,
    /// Accumulated `input_json_delta` for `tool_use` and `server_tool_use`.
    input_json: String,
    /// Slot in `tools` for a dispatchable `tool_use`.
    tool: Option<usize>,
    /// `content_block_stop` arrived.
    complete: bool,
}

/// Anthropic Messages: `message_start` carries the message envelope, each
/// `content_block_*` triple builds one content block, and `message_delta`
/// carries the stop reason and the final usage.
#[derive(Default)]
pub(super) struct AnthropicFold {
    message: Map<String, Value>,
    usage: Map<String, Value>,
    blocks: BTreeMap<u64, AnthropicBlock>,
    current: Option<u64>,
    tools: Vec<StreamToolCall>,
}

impl AnthropicFold {
    fn observe(&mut self, frame: &Value) -> Vec<StreamPart> {
        let index = frame.get("index").and_then(Value::as_u64).or(self.current);
        match frame["type"].as_str() {
            Some("message_start") => {
                let message = &frame["message"];
                merge_fields(&mut self.message, message, &["content", "usage"]);
                merge_fields(&mut self.usage, &message["usage"], &[]);
                Vec::new()
            }
            Some("content_block_start") => {
                let index = index.unwrap_or(self.blocks.len() as u64);
                self.current = Some(index);
                let mut block = frame["content_block"].clone();
                let mut tool = None;
                let mut parts = Vec::new();
                if block["type"] == "tool_use" {
                    let call = StreamToolCall::new(
                        block["id"].as_str().unwrap_or_default(),
                        block["name"].as_str().unwrap_or_default(),
                        self.tools.len() + 1,
                    );
                    block["id"] = json!(call.tool_call_id);
                    tool = Some(self.tools.len());
                    parts.push(StreamPart::ToolCall(self.tools.len()));
                    self.tools.push(call);
                }
                self.blocks.insert(
                    index,
                    AnthropicBlock {
                        block,
                        input_json: String::new(),
                        tool,
                        complete: false,
                    },
                );
                parts
            }
            Some("content_block_delta") => {
                let delta = &frame["delta"];
                let index = index.unwrap_or(self.blocks.len() as u64);
                // Text that arrives without its `content_block_start` is still
                // text the model produced.
                if delta["type"] == "text_delta" {
                    self.blocks.entry(index).or_insert_with(|| AnthropicBlock {
                        block: json!({"type": "text", "text": ""}),
                        input_json: String::new(),
                        tool: None,
                        complete: false,
                    });
                }
                let Some(entry) = self.blocks.get_mut(&index) else {
                    return Vec::new();
                };
                let append = |block: &mut Value, field: &str, text: &str| {
                    let joined = format!("{}{text}", block[field].as_str().unwrap_or_default());
                    block[field] = json!(joined);
                };
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append(&mut entry.block, "text", text);
                        vec![StreamPart::Text(text.to_string())]
                    }
                    Some("thinking_delta") => {
                        let thinking = delta["thinking"].as_str().unwrap_or_default();
                        append(&mut entry.block, "thinking", thinking);
                        if thinking.is_empty() {
                            Vec::new()
                        } else {
                            vec![StreamPart::Reasoning(thinking.to_string())]
                        }
                    }
                    Some("signature_delta") => {
                        append(
                            &mut entry.block,
                            "signature",
                            delta["signature"].as_str().unwrap_or_default(),
                        );
                        Vec::new()
                    }
                    Some("input_json_delta") => {
                        entry
                            .input_json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                        let Some(slot) = entry.tool else {
                            return Vec::new();
                        };
                        self.tools[slot].arguments.clone_from(&entry.input_json);
                        vec![StreamPart::ToolCall(slot)]
                    }
                    _ => Vec::new(),
                }
            }
            Some("content_block_stop") => {
                if let Some(entry) = index.and_then(|index| self.blocks.get_mut(&index)) {
                    entry.complete = true;
                    match entry.block["type"].as_str() {
                        Some("tool_use") => {
                            entry.block["input"] = parse_streamed_tool_input(&entry.input_json);
                        }
                        Some("server_tool_use") => {
                            entry.block["input"] = serde_json::from_str(&entry.input_json)
                                .unwrap_or_else(|_| json!({}));
                        }
                        _ => {}
                    }
                }
                self.current = None;
                Vec::new()
            }
            Some("message_delta") => {
                merge_fields(&mut self.message, &frame["delta"], &[]);
                merge_fields(&mut self.usage, &frame["usage"], &[]);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn into_body(self) -> Value {
        let mut message = self.message;
        // A block the stream never closed is not part of the response: a tool
        // call cut off mid-arguments must not dispatch. Text already streamed
        // to the caller stays.
        let content = self
            .blocks
            .into_values()
            .filter(|entry| entry.complete || entry.block["type"] == "text")
            .map(|entry| entry.block)
            .collect();
        message.insert("content".into(), Value::Array(content));
        message.insert("usage".into(), Value::Object(self.usage));
        Value::Object(message)
    }
}

/// Finalize the accumulated `input_json_delta` buffer for a streamed
/// `tool_use` block.
///
/// An empty buffer is a legitimate no-args call: the start block carries
/// `"input": {}` and no delta follows. A non-empty buffer that fails to parse
/// must not dispatch the tool with arguments the model never chose; it becomes
/// the `__parse_error` carrier the agent loop turns into invalid-arguments
/// feedback.
fn parse_streamed_tool_input(input_json: &str) -> Value {
    if input_json.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(input_json).unwrap_or_else(|error| {
        json!({
            "__parse_error": format!(
                "Could not parse streamed tool arguments as JSON: {}. Raw input: {}",
                error,
                preview_chars(input_json, 200)
            )
        })
    })
}

/// One `message.tool_calls` entry under construction, in arrival order.
enum OpenAiToolEntry {
    /// A dispatchable function call, by slot in `tools`.
    Function { slot: usize, provider_id: String },
    /// A provider-executed entry (`tool_search_call`, `tool_search_output`),
    /// complete in the frame that carried it.
    Provider(Value),
}

/// OpenAI Chat Completions: each chunk's `choices[0].delta` appends to one
/// assistant message, tool-call fragments join by `index`, and envelope fields
/// such as `usage` arrive on whichever chunk carries them.
#[derive(Default)]
pub(super) struct OpenAiFold {
    envelope: Map<String, Value>,
    /// `None` until a frame carries a `content` field.
    content: Option<String>,
    reasoning: String,
    /// A reasoning field appeared, even empty: a declared reasoning channel is
    /// a different response shape from none, and diagnostics report it.
    reasoning_declared: bool,
    finish_reason: Option<Value>,
    logprobs: Vec<Value>,
    entries: Vec<OpenAiToolEntry>,
    function_entries: BTreeMap<u64, usize>,
    tools: Vec<StreamToolCall>,
}

impl OpenAiFold {
    fn observe(&mut self, frame: &Value) -> Vec<StreamPart> {
        merge_fields(&mut self.envelope, frame, &["choices"]);
        let choice = &frame["choices"][0];
        let delta = &choice["delta"];
        let mut parts = Vec::new();
        if let Some(content) = delta["content"].as_str() {
            self.content.get_or_insert_default().push_str(content);
            if !content.is_empty() {
                parts.push(StreamPart::Text(content.to_string()));
            }
        }
        self.reasoning_declared |= ["reasoning", "reasoning_content", "reasoning_details"]
            .iter()
            .any(|field| delta.get(*field).is_some_and(|value| !value.is_null()));
        // Reasoning fields arrive as token-sized fragments and concatenate
        // verbatim; a newline between fragments would put one token per line.
        let reasoning = extract_openai_delta_field_str(
            delta,
            &["reasoning", "reasoning_content", "reasoning_details"],
        );
        self.reasoning.push_str(reasoning);
        if !reasoning.is_empty() {
            parts.push(StreamPart::Reasoning(reasoning.to_string()));
        }
        // Keep the first finish_reason: some routes repeat it on later frames
        // and the repeat would truncate an in-progress tool call (qwen-code#2402).
        if self.finish_reason.is_none() {
            if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
                self.finish_reason = Some(reason.clone());
            }
        }
        if let Some(tokens) = choice["logprobs"]["content"].as_array() {
            self.logprobs.extend(tokens.iter().cloned());
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            if matches!(
                call["type"].as_str(),
                Some("tool_search_call" | "tool_search_output")
            ) {
                // `index` frames the stream; it is not part of the entry.
                let mut entry = call.clone();
                if let Some(object) = entry.as_object_mut() {
                    object.remove("index");
                }
                self.entries.push(OpenAiToolEntry::Provider(entry));
                continue;
            }
            parts.push(StreamPart::ToolCall(self.observe_function(call)));
        }
        parts
    }

    fn observe_function(&mut self, call: &Value) -> usize {
        let index = call["index"].as_u64().unwrap_or(0);
        let id = call["id"].as_str().unwrap_or_default();
        let name = call["function"]["name"].as_str().unwrap_or_default();
        let slot = match self.function_entries.get(&index) {
            Some(&slot) => slot,
            None => {
                let slot = self.tools.len();
                self.tools
                    .push(StreamToolCall::new(id, name, index as usize + 1));
                self.function_entries.insert(index, slot);
                self.entries.push(OpenAiToolEntry::Function {
                    slot,
                    provider_id: id.to_string(),
                });
                slot
            }
        };
        // A route may split the call's metadata across deltas: the name on the
        // first, the id on a later one.
        let entry = self.entries.iter_mut().find_map(|entry| match entry {
            OpenAiToolEntry::Function {
                slot: entry_slot,
                provider_id,
            } if *entry_slot == slot => Some(provider_id),
            _ => None,
        });
        let tool = &mut self.tools[slot];
        if let Some(provider_id) = entry {
            if provider_id.is_empty() && !id.is_empty() {
                *provider_id = id.to_string();
                if !tool.published {
                    tool.tool_call_id = id.to_string();
                }
            }
        }
        if tool.name.is_empty() && !name.is_empty() {
            tool.name = name.to_string();
        }
        tool.arguments
            .push_str(call["function"]["arguments"].as_str().unwrap_or_default());
        slot
    }

    fn into_body(self) -> Value {
        let tool_calls: Vec<Value> = self
            .entries
            .into_iter()
            .map(|entry| match entry {
                OpenAiToolEntry::Function { slot, .. } => {
                    let tool = &self.tools[slot];
                    json!({
                        "id": tool.tool_call_id,
                        "type": "function",
                        "function": {"name": tool.name, "arguments": tool.arguments},
                    })
                }
                OpenAiToolEntry::Provider(call) => call,
            })
            .collect();
        let mut message = json!({"role": "assistant"});
        if let Some(content) = self.content {
            message["content"] = json!(content);
        }
        if self.reasoning_declared {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        let mut choice = json!({
            "index": 0,
            "message": message,
            "finish_reason": self.finish_reason.unwrap_or(Value::Null),
        });
        if !self.logprobs.is_empty() {
            choice["logprobs"] = json!({"content": self.logprobs});
        }
        let mut body = self.envelope;
        body.insert("choices".into(), json!([choice]));
        Value::Object(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_calls_fold_in_arrival_order_not_hash_order() {
        let mut fold = StreamFold::new(StreamProtocol::OpenAiSse);
        for (index, id) in [(1, "call_b"), (0, "call_a")] {
            fold.observe(&json!({"choices": [{"delta": {"tool_calls": [
                {"index": index, "id": id, "function": {"name": "read", "arguments": "{}"}}
            ]}}]}));
        }
        let body = fold.into_body();
        let ids: Vec<&str> = body["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["call_b", "call_a"], "arrival order is the body order");
    }

    #[test]
    fn a_late_provider_id_replaces_the_fallback_only_until_published() {
        let mut fold = StreamFold::new(StreamProtocol::OpenAiSse);
        let head = json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "function": {"name": "read", "arguments": ""}}
        ]}}]});
        let late_id = json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_late", "function": {"arguments": "{}"}}
        ]}}]});
        fold.observe(&head);
        fold.observe(&late_id);
        assert_eq!(fold.tool(0).tool_call_id, "call_late");

        let mut published = StreamFold::new(StreamProtocol::OpenAiSse);
        published.observe(&head);
        published.publish(0);
        let fallback = published.tool(0).tool_call_id.clone();
        published.observe(&late_id);
        assert_eq!(published.tool(0).tool_call_id, fallback);
        assert!(fallback.starts_with("stream-tool-1-"));
    }

    #[test]
    fn anthropic_usage_keeps_categories_a_later_frame_omits() {
        let mut fold = StreamFold::new(StreamProtocol::AnthropicSse);
        fold.observe(&json!({"type": "message_start", "message": {
            "id": "msg_1",
            "usage": {"input_tokens": 7, "cache_read_input_tokens": 0}
        }}));
        fold.observe(&json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 3, "cache_creation_input_tokens": 20}
        }));
        let body = fold.into_body();
        assert_eq!(
            body["usage"],
            json!({
                "input_tokens": 7,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 20,
                "output_tokens": 3,
            })
        );
        assert_eq!(body["stop_reason"], "end_turn");
    }

    #[test]
    fn an_unclosed_tool_block_is_not_part_of_the_response() {
        let mut fold = StreamFold::new(StreamProtocol::AnthropicSse);
        fold.observe(&json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}));
        fold.observe(&json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "partial"}}));
        fold.observe(&json!({"type": "content_block_start", "index": 1,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "edit"}}));
        let body = fold.into_body();
        assert_eq!(
            body["content"],
            json!([{"type": "text", "text": "partial"}])
        );
    }
}
