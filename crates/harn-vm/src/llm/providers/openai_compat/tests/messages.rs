//! Message-history normalization: tool-result adjacency, orphan and duplicate
//! results, parallel tool-call splitting, and image relocation.

use super::fixtures::{base_request_payload, part_is_image};
use crate::llm::providers::openai_compat::messages::{
    relocate_tool_message_images_to_user, split_parallel_native_tool_call_history,
};
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn tool_message_image_relocated_to_following_user_message() {
    // A computer-use tool result carries [text, image_url]. OpenAI rejects an
    // image on a role:"tool" message, so the image must move to a user turn.
    let msgs = vec![
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1"}]}),
        json!({
            "role": "tool",
            "tool_call_id": "c1",
            "content": [
                {"type": "text", "text": "Screenshot captured."},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
            ],
        }),
    ];
    let out = relocate_tool_message_images_to_user(msgs);
    assert_eq!(
        out.len(),
        3,
        "one user message inserted after the tool result"
    );
    // The tool message keeps only its text part, no image.
    let tool = &out[1];
    assert_eq!(tool["role"], "tool");
    let tool_parts = tool["content"].as_array().expect("tool content array");
    assert!(
        tool_parts
            .iter()
            .all(|p| p.get("type").and_then(|t| t.as_str()) != Some("image_url")),
        "tool message must not carry an image"
    );
    assert_eq!(tool_parts[0]["text"], "Screenshot captured.");
    // The image lands on a following user message.
    let user = &out[2];
    assert_eq!(user["role"], "user");
    let user_parts = user["content"].as_array().expect("user content array");
    assert!(user_parts
        .iter()
        .any(|p| p.get("type").and_then(|t| t.as_str()) == Some("image_url")));
}

#[test]
fn tool_message_without_image_is_untouched() {
    let msgs = vec![json!({
        "role": "tool",
        "tool_call_id": "c1",
        "content": [{"type": "text", "text": "plain result"}],
    })];
    let out = relocate_tool_message_images_to_user(msgs.clone());
    assert_eq!(out, msgs, "no image parts -> no split, order preserved");
}

#[test]
fn strict_provider_defers_feedback_until_after_tool_result() {
    let mut payload = base_request_payload();
    payload.provider = "minimax".to_string();
    payload.model = "MiniMax-M2".to_string();
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"main.rs\"}"},
            }],
        }),
        json!({"role": "user", "content": "[runtime_feedback] keep going"}),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "fn main() {}"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    let roles = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or("?"))
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "tool", "user"]);
    assert_eq!(messages[2]["tool_call_id"], "call_001");
    assert_eq!(messages[3]["content"], "[runtime_feedback] keep going");
}

#[test]
fn strict_provider_keeps_parallel_tool_results_adjacent() {
    let mut payload = base_request_payload();
    payload.provider = "moonshot".to_string();
    payload.model = "moonshot/kimi-k2.6".to_string();
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {
                    "id": "call_001",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"},
                },
                {
                    "id": "call_002",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"b.rs\"}"},
                },
            ],
        }),
        json!({"role": "user", "content": "[runtime_feedback] keep going"}),
        json!({"role": "tool", "tool_call_id": "call_002", "content": "b"}),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "a"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    let roles = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or("?"))
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "user"]);
    assert_eq!(messages[2]["tool_call_id"], "call_002");
    assert_eq!(messages[3]["tool_call_id"], "call_001");
    assert_eq!(messages[4]["content"], "[runtime_feedback] keep going");
}

#[test]
fn native_route_drops_orphan_tool_result_messages() {
    let mut payload = base_request_payload();
    payload.provider = "groq".to_string();
    payload.model = "openai/gpt-oss-120b".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file.",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        }
    })]);
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({"role": "tool", "tool_call_id": "stale_call", "content": "stale compacted result"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"},
            }],
        }),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "fresh result"}),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "duplicate result"}),
        json!({"role": "user", "content": "continue"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    let roles = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or("?"))
        .collect::<Vec<_>>();

    assert_eq!(roles, vec!["user", "assistant", "tool", "user"]);
    assert_eq!(messages[2]["tool_call_id"], "call_001");
    assert_eq!(messages[2]["content"], "fresh result");
    assert!(
        messages
            .iter()
            .all(|message| message["content"] != "stale compacted result"
                && message["content"] != "duplicate result"),
        "orphaned or duplicate tool results must not reach native tool providers: {messages:?}"
    );
}

#[test]
fn single_tool_call_text_route_strips_native_tool_history_metadata() {
    let mut payload = base_request_payload();
    payload.provider = "fireworks".to_string();
    payload.model = "accounts/fireworks/models/gpt-oss-120b".to_string();
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({
            "role": "assistant",
            "content": "<tool_call>\nread({ path: \"a.rs\" })\n</tool_call>\n<tool_call>\nread({ path: \"b.rs\" })\n</tool_call>",
            "tool_calls": [
                {
                    "id": "call_001",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"},
                },
                {
                    "id": "call_002",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"b.rs\"}"},
                },
            ],
        }),
        json!({"role": "tool", "tool_call_id": "call_001", "name": "read", "content": "a"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(body.get("parallel_tool_calls").is_none());
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages[1]["role"], "assistant");
    assert!(messages[1]["content"]
        .as_str()
        .expect("assistant content")
        .contains("read({ path: \"a.rs\" })"));
    assert_eq!(messages[2]["role"], "user");
    assert!(messages[2].get("tool_call_id").is_none());
    assert!(messages[2].get("name").is_none());
    assert!(
        messages
            .iter()
            .all(|message| message.get("tool_calls").is_none()),
        "text-tool routes must not send native tool_calls history to Fireworks: {messages:?}"
    );
}

#[test]
fn single_tool_call_native_route_splits_parallel_tool_history() {
    let mut payload = base_request_payload();
    payload.provider = "fireworks".to_string();
    payload.model = "accounts/fireworks/models/gpt-oss-120b".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        }
    })]);
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {
                    "id": "call_001",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"},
                },
                {
                    "id": "call_002",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"b.rs\"}"},
                },
            ],
        }),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "a"}),
        json!({"role": "tool", "tool_call_id": "call_002", "content": "b"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["parallel_tool_calls"], false);
    let messages = body["messages"].as_array().expect("messages array");
    let roles = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or("?"))
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec!["user", "assistant", "tool", "assistant", "tool"]
    );
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_001");
    assert_eq!(messages[2]["tool_call_id"], "call_001");
    assert_eq!(messages[3]["tool_calls"][0]["id"], "call_002");
    assert_eq!(messages[4]["tool_call_id"], "call_002");
    assert_eq!(
        messages[1]["tool_calls"]
            .as_array()
            .expect("first calls")
            .len(),
        1
    );
    assert_eq!(
        messages[3]["tool_calls"]
            .as_array()
            .expect("second calls")
            .len(),
        1
    );
}

#[test]
fn lenient_provider_preserves_interleaved_tool_feedback_order() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-4o".to_string();
    payload.messages = vec![
        json!({"role": "user", "content": "inspect"}),
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"main.rs\"}"},
            }],
        }),
        json!({"role": "user", "content": "[runtime_feedback] keep going"}),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "fn main() {}"}),
    ];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    let roles = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap_or("?"))
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["user", "assistant", "user", "tool"]);
}

#[test]
fn relocate_images_keeps_parallel_tool_results_adjacent() {
    // A parallel tool-call batch whose FIRST tool result carries a
    // screenshot: the relocated `user` image message must land AFTER the
    // whole run of tool messages, never between them. OpenAI rejects a
    // non-tool message interleaved with the tool results answering an
    // assistant's `tool_calls`, so an inline insert would 400 the turn.
    let msgs = vec![
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "c1"}, {"id": "c2"}],
        }),
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "c1",
            "content": [
                {"type": "text", "text": "shot"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
            ],
        }),
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "c2",
            "content": [{"type": "text", "text": "read ok"}],
        }),
        serde_json::json!({"role": "assistant", "content": "done"}),
    ];
    let out = relocate_tool_message_images_to_user(msgs);
    let roles: Vec<&str> = out
        .iter()
        .map(|m| m.get("role").and_then(|v| v.as_str()).unwrap_or(""))
        .collect();
    // assistant, tool(c1 text-only), tool(c2), user(image), assistant.
    assert_eq!(
        roles,
        ["assistant", "tool", "tool", "user", "assistant"],
        "images must flush after the contiguous tool-result run: {out:?}"
    );
    // The image rode onto the user message, stripped off the tool result.
    let user = out.iter().find(|m| m["role"] == "user").unwrap();
    assert!(
        user["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(part_is_image),
        "relocated user message must carry the image: {user}"
    );
    assert!(
        !out[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(part_is_image),
        "image must be stripped off the tool result: {}",
        out[1]
    );
}

#[test]
fn split_parallel_tool_calls_attaches_results_by_id_not_position() {
    // Regression: a parallel assistant batch whose MIDDLE call lacks an id.
    // Tool results must stay keyed by id — a positional index into the
    // id-compacted `ids` vec would misattach the third call's result to the
    // id-less middle call.
    let msgs = vec![
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                {"type": "function", "function": {"name": "b", "arguments": "{}"}},
                {"id": "c3", "type": "function", "function": {"name": "c", "arguments": "{}"}},
            ],
        }),
        json!({"role": "tool", "tool_call_id": "c1", "content": "result-a"}),
        json!({"role": "tool", "tool_call_id": "c3", "content": "result-c"}),
    ];

    let out = split_parallel_native_tool_call_history(msgs);

    // Every tool result must immediately follow the single-call assistant
    // message whose id it answers.
    let mut seen_a = false;
    let mut seen_c = false;
    for pair in out.windows(2) {
        let (first, second) = (&pair[0], &pair[1]);
        if second["role"] == "tool" {
            let result_id = second["tool_call_id"].as_str().expect("tool_call_id");
            let call_id = first["tool_calls"][0]["id"].as_str().unwrap_or("");
            assert_eq!(
                call_id, result_id,
                "tool result {result_id} attached to wrong call {call_id}: {out:?}"
            );
            seen_a |= result_id == "c1";
            seen_c |= result_id == "c3";
        }
    }
    assert!(seen_a && seen_c, "both results must be present: {out:?}");

    // The id-less middle call gets its own assistant message with no result.
    let middle = out
        .iter()
        .find(|m| m["tool_calls"][0]["function"]["name"] == "b")
        .expect("middle call present");
    assert!(middle["tool_calls"][0].get("id").is_none());
}
