use super::super::anthropic_test_support::base_payload;
use super::*;
use crate::llm::api::{LlmErrorKind, LlmErrorReason};

#[test]
fn stored_reasoning_key_stripped_from_outgoing_messages() {
    // Reproduces the eval-traced HTTP 400: a persisted assistant turn
    // carries a top-level `reasoning` key (stamped by
    // build_assistant_response_message) which, if echoed into the
    // Anthropic request, returns
    // `messages.1.reasoning: Extra inputs are not permitted`.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "do the task"}),
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "reasoning", "text": "hidden chain", "visibility": "private"},
                {"type": "text", "text": "Let me start by understanding..."}
            ],
            "reasoning": "Let me start by understanding the task.",
            // OpenAI-shape leakage and other storage-only metadata must also
            // be stripped at the Anthropic egress boundary.
            "tool_calls": [{"id": "x", "type": "function"}],
            "private_reasoning": "hidden",
        }),
        serde_json::json!({"role": "user", "content": "continue"}),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    // The persisted assistant turn (messages[1]) must carry ONLY
    // canonical Anthropic message keys after projection.
    let assistant = messages[1].as_object().expect("assistant object");
    assert!(
        assistant.get("reasoning").is_none(),
        "non-canonical `reasoning` key rode into the Anthropic request: {assistant:?}"
    );
    assert!(assistant.get("tool_calls").is_none());
    assert!(assistant.get("private_reasoning").is_none());
    assert_eq!(
        assistant.get("role").and_then(|v| v.as_str()),
        Some("assistant")
    );
    assert!(
        assistant.get("content").is_some(),
        "content must be preserved (replay/answer continuity)"
    );
    assert!(
        !assistant["content"].to_string().contains("hidden chain"),
        "private reasoning content block rode into the Anthropic request: {assistant:?}"
    );

    // Replay-preservation: the SOURCE transcript shape is untouched —
    // build_request_body must not mutate opts.messages in place.
    assert_eq!(
        opts.messages[1].get("reasoning").and_then(|v| v.as_str()),
        Some("Let me start by understanding the task."),
        "persisted transcript shape must be unchanged at the storage layer"
    );

    // Canonical round-trip: plain user/assistant turns with no stored
    // metadata still serialize their content unchanged.
    assert_eq!(
        messages[0].get("content").and_then(|v| v.as_str()),
        Some("do the task")
    );
}

#[test]
fn cross_provider_tool_role_message_translated_to_anthropic_shape() {
    // Reproduces the cross-provider escalation HTTP 400
    // (`messages: Unexpected role "tool"`): a cheap OpenAI/Ollama-dialect
    // primary records tool results as top-level `role:"tool"` messages.
    // When escalation switches the provider to Anthropic and replays that
    // history, Anthropic rejects `role:"tool"` — it wants a `role:"user"`
    // message carrying a `tool_result` content block. Pre-fix, the
    // `role:"tool"` message rides through verbatim (its `tool_call_id` is
    // even stripped by the canonical-key retain), producing the 400.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "read the file"}),
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "call_001", "name": "read_file", "input": {}}
            ],
        }),
        // OpenAI dialect tool-result carried forward from the primary.
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "call_001",
            "name": "read_file",
            "content": "fn main() {}",
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    // No top-level `role:"tool"` message may survive to the Anthropic wire.
    assert!(
        messages
            .iter()
            .all(|m| m.get("role").and_then(|r| r.as_str()) != Some("tool")),
        "a top-level role:\"tool\" message rode into the Anthropic request: {messages:?}"
    );

    // The tool result must now be a `role:"user"` message carrying a
    // `tool_result` block keyed by the matching tool_use_id, and the real
    // observation must be preserved (not masked by an interrupted-before-
    // dispatch placeholder).
    let tool_result_block = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .expect("a user message carrying a tool_result block");
    assert_eq!(
        tool_result_block
            .get("tool_use_id")
            .and_then(|v| v.as_str()),
        Some("call_001"),
        "tool_result must key off the original tool_call_id"
    );
    assert_eq!(
        tool_result_block.get("content").and_then(|v| v.as_str()),
        Some("fn main() {}"),
        "the real observation must survive (no placeholder masking)"
    );

    // Replay-preservation: the source transcript shape is untouched.
    assert_eq!(
        opts.messages[2].get("role").and_then(|v| v.as_str()),
        Some("tool"),
        "persisted transcript shape must be unchanged at the storage layer"
    );
}

#[test]
fn canonical_tool_call_and_result_pair_project_for_anthropic() {
    // Reproduces the THIRD stacked escalation 400 (downstream of the
    // role:"tool" fix): `messages.N.content.M: unexpected tool_use_id found
    // in tool_result blocks: <id>. Each tool_result block must have a
    // corresponding tool_use.` The primary's OpenAI-dialect assistant turn
    // carries its calls as a top-level `tool_calls` array (with BOTH text
    // content AND the call). The role:"tool" fix translates the RESULT half
    // into a tool_result block keyed by that id — but pre-fix the assistant
    // `tool_calls` were STRIPPED by the canonical-key retain, leaving the
    // tool_result orphaned. Both halves must translate so the pair matches.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "read the file"}),
        // Provider-neutral assistant turn: text + a canonical tool call.
        serde_json::json!({
            "role": "assistant",
            "content": "I'll read it now.",
            "tool_calls": [{
                "id": "call_R0hU",
                "name": "read_file",
                "arguments": {"path": "main.rs"},
            }],
        }),
        // Provider-neutral tool result referencing that call id.
        serde_json::json!({
            "role": "tool_result",
            "tool_call_id": "call_R0hU",
            "name": "read_file",
            "content": "fn main() {}",
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    // The assistant message must carry a tool_use block (id call_R0hU) with
    // the parsed input object, AND preserve its leading text block.
    let assistant = messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .expect("assistant message present");
    assert!(
        assistant.get("tool_calls").is_none(),
        "top-level OpenAI `tool_calls` must not ride into the Anthropic request: {assistant:?}"
    );
    let assistant_blocks = assistant
        .get("content")
        .and_then(|c| c.as_array())
        .expect("assistant content is a block list");
    let tool_use = assistant_blocks
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .expect("a tool_use block in the assistant message");
    assert_eq!(
        tool_use.get("id").and_then(|v| v.as_str()),
        Some("call_R0hU")
    );
    assert_eq!(
        tool_use.get("name").and_then(|v| v.as_str()),
        Some("read_file")
    );
    assert_eq!(
        tool_use.get("input"),
        Some(&serde_json::json!({"path": "main.rs"})),
        "OpenAI `arguments` string must be parsed into Anthropic `input` object"
    );
    assert!(
        assistant_blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")),
        "assistant text content must be preserved alongside the tool_use: {assistant_blocks:?}"
    );

    // The matching tool_result must exist keyed by the SAME id — and NOT be
    // the interrupted-before-dispatch placeholder (the real result survives).
    let tool_result = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .expect("a tool_result block");
    assert_eq!(
        tool_result.get("tool_use_id").and_then(|v| v.as_str()),
        Some("call_R0hU"),
        "tool_result must pair with the assistant tool_use id"
    );
    assert_eq!(
        tool_result.get("content").and_then(|v| v.as_str()),
        Some("fn main() {}"),
        "real observation must survive (no placeholder masking)"
    );
    // Full-pairing invariant: every tool_result id has a corresponding
    // tool_use id — no orphan, which is exactly what Anthropic 400s on.
    let tool_use_ids: std::collections::BTreeSet<String> = messages
        .iter()
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| b.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    let tool_result_ids: std::collections::BTreeSet<String> = messages
        .iter()
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .filter_map(|b| {
            b.get("tool_use_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    assert!(
            tool_result_ids.is_subset(&tool_use_ids),
            "orphaned tool_result id (no corresponding tool_use): results={tool_result_ids:?} uses={tool_use_ids:?}"
        );
}

#[test]
fn top_level_tool_result_role_message_translated_to_anthropic_shape() {
    // Defense-in-depth: a synthesized tool-result whose role is the
    // Anthropic-native `tool_result` string (what
    // tool_result_message_for_provider emits) would ALSO 400 if it reached
    // egress as a top-level message. The same choke point must fold it into
    // a role:"user" + tool_result block.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "read the file"}),
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "toolu_9", "name": "read_file", "input": {}}
            ],
        }),
        serde_json::json!({
            "role": "tool_result",
            "tool_use_id": "toolu_9",
            "content": "fn main() {}",
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");
    assert!(
        messages.iter().all(|m| {
            let r = m.get("role").and_then(|r| r.as_str());
            r != Some("tool") && r != Some("tool_result")
        }),
        "a top-level tool-result role rode into the Anthropic request: {messages:?}"
    );
    let block = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .expect("a user message carrying a tool_result block");
    assert_eq!(
        block.get("tool_use_id").and_then(|v| v.as_str()),
        Some("toolu_9")
    );
    assert_eq!(
        block.get("content").and_then(|v| v.as_str()),
        Some("fn main() {}")
    );
}

#[test]
fn homogeneous_anthropic_tool_result_unchanged_by_translation() {
    // Guard: a message history already in Anthropic shape (role:"user"
    // with a tool_result block) must be byte-identical before and after —
    // the translation only touches literal role:"tool" messages.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "read the file"}),
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "toolu_001", "name": "read_file", "input": {}}
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "toolu_001", "content": "fn main() {}"}
            ],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");
    // The tool_result user message survives with its id and content intact,
    // and no placeholder/backfill was synthesized (exactly one tool_result).
    let tool_results: Vec<_> = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .flat_map(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .collect();
    assert_eq!(
        tool_results.len(),
        1,
        "no duplicate/placeholder tool_result"
    );
    assert_eq!(
        tool_results[0].get("tool_use_id").and_then(|v| v.as_str()),
        Some("toolu_001")
    );
    assert_eq!(
        tool_results[0].get("content").and_then(|v| v.as_str()),
        Some("fn main() {}")
    );
}

#[test]
fn whitespace_is_dropped_and_unpaired_tool_result_is_preserved_as_text() {
    let mut opts = base_payload();
    opts.messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "  \n\t"},
            {"type": "text", "text": "keep me"},
            {
                "type": "tool_result",
                "tool_use_id": "toolu_read",
                "content": "result"
            }
        ],
    })];

    let body = AnthropicProvider::build_request_body(&opts);
    let content = body["messages"][0]["content"]
        .as_array()
        .expect("content blocks");

    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "keep me");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "[unpaired durable tool result]\nresult");
}

#[test]
fn whitespace_only_messages_are_dropped_before_tool_result_adjacency() {
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_verify",
                "name": "verify",
                "input": {},
            }],
        }),
        serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "\n   \t"}],
        }),
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_verify",
                "content": "ok",
            }],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"][0]["type"], "tool_result");
    assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_verify");
}

#[test]
fn injected_feedback_deferred_until_after_tool_result() {
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I will verify."},
                {
                    "type": "tool_use",
                    "id": "toolu_verify",
                    "name": "verify",
                    "input": {},
                },
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "<runtime_feedback>grounding note</runtime_feedback>"}
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_verify",
                    "content": "tests passed",
                }
            ],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(
        messages[0]["content"][1],
        serde_json::json!({
            "type": "tool_use",
            "id": "toolu_verify",
            "name": "verify",
            "input": {},
        })
    );
    assert_eq!(
        messages[1]["content"][0],
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "toolu_verify",
            "content": "tests passed",
        })
    );
    assert_eq!(
        messages[2]["content"][0],
        serde_json::json!({
            "type": "text",
            "text": "<runtime_feedback>grounding note</runtime_feedback>",
        })
    );
}

#[test]
fn feedback_deferred_when_tool_use_is_not_final_content_block() {
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_verify",
                    "name": "verify",
                    "input": {},
                },
                {"type": "text", "text": "Waiting for the result."},
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "<runtime_feedback>late reminder</runtime_feedback>"}
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_verify",
                    "content": "ok",
                }
            ],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(
        messages[1]["content"][0],
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "toolu_verify",
            "content": "ok",
        })
    );
    assert_eq!(
        messages[2]["content"][0],
        serde_json::json!({
            "type": "text",
            "text": "<runtime_feedback>late reminder</runtime_feedback>",
        })
    );
}

#[test]
fn orphaned_tool_use_gets_placeholder_tool_result_backfill() {
    // A transcript that ends on an assistant tool_use with no recorded
    // tool_result (e.g. an interrupt/suspend path that failed to close
    // out its calls) must not 400 the whole session: the egress
    // normalizer backfills a placeholder result.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "do the thing"}),
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_orphan",
                "name": "run",
                "input": {},
            }],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"][0],
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "toolu_orphan",
            "content": "result unavailable (interrupted before dispatch)",
            "is_error": true,
        })
    );
}

#[test]
fn orphaned_tool_use_backfill_lands_before_deferred_user_text() {
    // An orphaned tool_use followed by injected user feedback: the
    // placeholder result must sit ADJACENT to the assistant turn, with
    // the deferred user text after it — the same ordering contract the
    // real-result reorder path guarantees.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_orphan",
                "name": "run",
                "input": {},
            }],
        }),
        serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": "STOP — user interrupted"}],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["content"][0]["type"], "tool_result");
    assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_orphan");
    assert_eq!(
        messages[2]["content"][0],
        serde_json::json!({"type": "text", "text": "STOP — user interrupted"})
    );
}

#[test]
fn partially_orphaned_tool_use_backfills_only_missing_ids() {
    // Two parallel tool_use blocks, only one real result: the backfill
    // must cover exactly the missing id (sorted, deterministic) and
    // leave the real result untouched.
    let mut opts = base_payload();
    opts.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "toolu_b", "name": "run", "input": {}},
                {"type": "tool_use", "id": "toolu_a", "name": "read", "input": {}},
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_a",
                "content": "file text",
            }],
        }),
    ];

    let body = AnthropicProvider::build_request_body(&opts);
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_a");
    assert_eq!(messages[1]["content"][0]["content"], "file text");
    let backfill = messages[2]["content"].as_array().expect("backfill blocks");
    assert_eq!(backfill.len(), 1);
    assert_eq!(backfill[0]["tool_use_id"], "toolu_b");
    assert_eq!(backfill[0]["is_error"], true);
}

#[test]
fn tool_search_supported_for_claude_4_opus_and_up() {
    // Per Anthropic's tool-search docs:
    //   Claude Mythos Preview, Sonnet 4.0+, Opus 4.0+, Haiku 4.5+.
    assert!(claude_model_supports_tool_search("claude-opus-4-7"));
    assert!(claude_model_supports_tool_search("claude-opus-4.7"));
    assert!(claude_model_supports_tool_search("claude-opus-4-0"));
    assert!(claude_model_supports_tool_search("claude-sonnet-4-6"));
    assert!(claude_model_supports_tool_search("claude-sonnet-4-0"));
}

#[test]
fn tool_search_unsupported_for_older_claude() {
    // Opus/Sonnet 3.x predate the feature.
    assert!(!claude_model_supports_tool_search("claude-opus-3-5"));
    assert!(!claude_model_supports_tool_search("claude-sonnet-3-5"));
    assert!(!claude_model_supports_tool_search("claude-haiku-3-5"));
}

#[test]
fn tool_search_haiku_requires_4_5() {
    // Haiku's cutoff is 4.5 (later than Opus/Sonnet's 4.0).
    assert!(!claude_model_supports_tool_search("claude-haiku-4-0"));
    assert!(!claude_model_supports_tool_search("claude-haiku-4-4"));
    assert!(claude_model_supports_tool_search("claude-haiku-4-5"));
    assert!(claude_model_supports_tool_search(
        "claude-haiku-4-5-20251001"
    ));
    assert!(claude_model_supports_tool_search("claude-haiku-5-0"));
}

#[test]
fn tool_search_unsupported_for_non_claude() {
    assert!(!claude_model_supports_tool_search("gpt-5"));
    assert!(!claude_model_supports_tool_search("gpt-5.4-turbo"));
    assert!(!claude_model_supports_tool_search("gemini-2.0"));
    assert!(!claude_model_supports_tool_search(""));
}

#[test]
fn native_tool_search_variants_lists_bm25_first() {
    let provider = AnthropicProvider;
    let variants = provider.native_tool_search_variants("claude-opus-4-7");
    assert_eq!(variants, vec!["bm25".to_string(), "regex".to_string()]);
}

#[test]
fn native_tool_search_variants_empty_for_old_model() {
    let provider = AnthropicProvider;
    assert!(provider
        .native_tool_search_variants("claude-opus-3-5")
        .is_empty());
}

#[test]
fn supports_defer_loading_matches_tool_search_gate() {
    let provider = AnthropicProvider;
    assert!(provider.supports_defer_loading("claude-opus-4-7"));
    assert!(!provider.supports_defer_loading("claude-opus-3-5"));
}

#[test]
fn fast_tier_injects_speed_knob_and_beta_header() {
    // `fast: true` on a model whose catalog tier rides `speed` sets the
    // top-level request knob and the beta header flows through the
    // payload's resolved Anthropic beta features.
    let mut payload = base_payload();
    payload.model = "claude-opus-4-8".to_string();
    payload.fast = true;
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(body["speed"], serde_json::json!("fast"));

    let opts = {
        let mut o = crate::llm::api::options::base_opts("anthropic");
        o.model = "claude-opus-4-8".to_string();
        o.fast = true;
        o
    };
    assert!(
        opts.anthropic_beta_features_for_request()
            .iter()
            .any(|f| f == "fast-mode-2026-02-01"),
        "fast mode must request the fast-mode beta header"
    );
}

#[test]
fn fast_tier_knob_absent_when_off() {
    let mut payload = base_payload();
    payload.model = "claude-opus-4-8".to_string();
    payload.fast = false;
    let body = AnthropicProvider::build_request_body(&payload);
    assert!(body.get("speed").is_none());
}

#[test]
fn native_tools_strip_harn_internal_extensions() {
    let mut payload = base_payload();
    payload.native_tools = Some(vec![serde_json::json!({
        "name": "read_file",
        "description": "Read a file",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "pattern": "^/",
                    "format": "uri-reference",
                    "minLength": 1
                }
            }
        },
        "x-harn-output-schema": {"type": "object"},
        "defer_loading": true,
        "namespace": "fs",
    })]);
    let body = AnthropicProvider::build_request_body(&payload);
    let sent = body["tools"][0].as_object().expect("tool object");
    assert!(
        !sent.contains_key("x-harn-output-schema"),
        "Anthropic rejects unknown tool fields with HTTP 400; the x-harn-output-schema \
             extension must be stripped before sending"
    );
    assert!(!sent.contains_key("defer_loading"));
    assert!(!sent.contains_key("namespace"));
    assert!(sent.contains_key("input_schema"));
    assert_eq!(sent["input_schema"]["additionalProperties"], false);
    assert!(sent["input_schema"]["properties"]["path"]
        .get("pattern")
        .is_none());
    assert!(sent["input_schema"]["properties"]["path"]
        .get("format")
        .is_none());
    assert!(sent["input_schema"]["properties"]["path"]
        .get("minLength")
        .is_none());
}

#[test]
fn output_format_json_schema_uses_native_format_with_thinking_and_tools() {
    let mut payload = base_payload();
    payload.model = "claude-haiku-4-5".to_string();
    payload.thinking = crate::llm::api::ThinkingConfig::Enabled {
        budget_tokens: Some(1024),
    };
    payload.prefill = Some("begin JSON".to_string());
    payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string", "pattern": "^ok"}},
            "required": ["answer"],
        }),
        strict: true,
    };

    let body = AnthropicProvider::build_request_body(&payload);

    assert_eq!(
        body["output_config"]["format"]["type"],
        serde_json::json!("json_schema")
    );
    let sent_schema = &body["output_config"]["format"]["schema"];
    assert_eq!(sent_schema["properties"]["answer"]["type"], "string");
    assert_eq!(sent_schema["additionalProperties"], false);
    assert!(sent_schema["properties"]["answer"].get("pattern").is_none());
    assert_eq!(
        body["thinking"],
        serde_json::json!({"type": "enabled", "budget_tokens": 1024}),
        "native schema output must preserve the requested reasoning mode"
    );
    assert!(
        body.get("tool_choice").is_none(),
        "native schema output must not force a synthetic tool"
    );
    assert!(
        body["messages"].as_array().is_some_and(|messages| messages
            .iter()
            .all(|message| message["content"] != "begin JSON")),
        "Anthropic rejects assistant prefill alongside native schema output"
    );
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1, "the caller's tool surface stays reachable");
    assert_eq!(tools[0]["name"], "read_file");
}

#[test]
fn forced_json_overrides_caller_tools_and_tool_choice() {
    // Caller supplies their own native tool AND tool_choice, then also asks
    // for structured output. Structured output wins (the documented
    // precedence) instead of silently leaving the caller's tool_choice in
    // place: tool_choice is pinned to json_response and that tool is added,
    // while the caller's tool is preserved in the array (just unreachable).
    let mut payload = base_payload();
    payload.native_tools = Some(vec![serde_json::json!({
        "name": "lookup",
        "description": "look something up",
        "input_schema": {"type": "object"},
    })]);
    payload.tool_choice = Some(serde_json::json!({"type": "auto"}));
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;

    let body = AnthropicProvider::build_request_body(&payload);

    assert_eq!(
        body["tool_choice"],
        serde_json::json!({"type": "tool", "name": "json_response"}),
        "structured output must win over the caller's tool_choice"
    );
    let tool_names: Vec<&str> = body["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
        .collect();
    assert!(tool_names.contains(&"json_response"));
    assert!(
        tool_names.contains(&"lookup"),
        "the caller's tool is preserved, not dropped: {tool_names:?}"
    );
}

#[test]
fn tool_choice_string_modes_become_anthropic_objects() {
    // The OpenAI/agent-loop wire shape is a bare string. Anthropic 400s on
    // a bare string, so each mode must be rewritten to its object form.
    for (input, expected) in [
        ("auto", serde_json::json!({"type": "auto"})),
        ("any", serde_json::json!({"type": "any"})),
        ("required", serde_json::json!({"type": "any"})),
        ("none", serde_json::json!({"type": "none"})),
    ] {
        let mut payload = base_payload();
        payload.tool_choice = Some(serde_json::json!(input));
        let body = AnthropicProvider::build_request_body(&payload);
        assert_eq!(
            body["tool_choice"], expected,
            "tool_choice \"{input}\" must serialize to an object"
        );
        assert!(
            body["tool_choice"].is_object(),
            "Anthropic rejects a non-object tool_choice"
        );
    }
}

#[test]
fn tool_choice_bare_string_names_a_specific_tool() {
    // A non-keyword bare string is treated as "force this tool by name".
    let mut payload = base_payload();
    payload.tool_choice = Some(serde_json::json!("read_file"));
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["tool_choice"],
        serde_json::json!({"type": "tool", "name": "read_file"})
    );
}

#[test]
fn tool_choice_openai_function_object_maps_to_anthropic_tool() {
    // OpenAI's specific-tool shape is `{"type":"function","function":{...}}`.
    let mut payload = base_payload();
    payload.tool_choice = Some(serde_json::json!({
        "type": "function",
        "function": {"name": "read_file"},
    }));
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["tool_choice"],
        serde_json::json!({"type": "tool", "name": "read_file"})
    );
}

#[test]
fn tool_choice_already_anthropic_object_is_preserved() {
    // Callers that already speak Anthropic must pass through unchanged,
    // including the optional disable_parallel_tool_use flag.
    let mut payload = base_payload();
    payload.tool_choice = Some(serde_json::json!({
        "type": "tool",
        "name": "read_file",
        "disable_parallel_tool_use": true,
    }));
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["tool_choice"],
        serde_json::json!({
            "type": "tool",
            "name": "read_file",
            "disable_parallel_tool_use": true,
        })
    );
}

#[test]
fn tool_choice_null_leaves_field_unset() {
    let mut payload = base_payload();
    payload.tool_choice = Some(serde_json::Value::Null);
    let body = AnthropicProvider::build_request_body(&payload);
    assert!(
        body.get("tool_choice").is_none(),
        "a null tool_choice must not be forwarded"
    );
}

#[test]
fn classifies_anthropic_overloaded_error_as_transient_server_error() {
    let info = crate::llm::api::DialectContract::new(
        crate::llm::capabilities::WireDialect::Anthropic,
        None,
    )
    .classify_http_error(
        "anthropic",
        reqwest::StatusCode::from_u16(529).unwrap(),
        None,
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::ServerError);
}

#[test]
fn classifies_anthropic_auth_error_as_terminal_auth_failure() {
    let info = crate::llm::api::DialectContract::new(
        crate::llm::capabilities::WireDialect::Anthropic,
        None,
    )
    .classify_http_error(
        "anthropic",
        reqwest::StatusCode::UNAUTHORIZED,
        None,
        r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::AuthFailure);
}

#[test]
fn image_content_maps_to_anthropic_source_block() {
    let mut payload = base_payload();
    payload.messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "caption"},
            {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png"}
        ],
    })];

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(body["messages"][0]["content"][0]["text"], "caption");
    assert_eq!(
        body["messages"][0]["content"][1],
        serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0KGgo=",
            }
        })
    );
}

#[test]
fn image_url_content_maps_to_anthropic_url_source() {
    let mut payload = base_payload();
    payload.messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png"}
        ],
    })];

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["messages"][0]["content"][0],
        serde_json::json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": "https://example.com/image.png",
            }
        })
    );
}

#[test]
fn pdf_file_id_content_maps_to_anthropic_document_block() {
    let mut payload = base_payload();
    payload.messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "pdf", "file_id": "file_123", "title": "Report"}
        ],
    })];

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["messages"][0]["content"][0],
        serde_json::json!({
            "type": "document",
            "source": {
                "type": "file",
                "file_id": "file_123",
            },
            "title": "Report",
        })
    );
}

#[test]
fn audio_base64_content_maps_to_anthropic_audio_block() {
    let mut payload = base_payload();
    payload.messages = vec![serde_json::json!({
        "role": "user",
        "content": [
            {"type": "audio", "base64": "UklGRg==", "media_type": "audio/wav"}
        ],
    })];

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["messages"][0]["content"][0],
        serde_json::json!({
            "type": "audio",
            "source": {
                "type": "base64",
                "media_type": "audio/wav",
                "data": "UklGRg==",
            }
        })
    );
}

#[test]
fn cache_uses_top_level_automatic_prompt_caching() {
    let mut payload = base_payload();
    payload.cache = true;

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );
    assert_eq!(body["system"].as_str(), Some("system prompt"));
    assert_eq!(
        body["tools"].as_array().map(Vec::len),
        Some(1),
        "tool definitions remain in the top-level cached prefix"
    );
}

#[test]
fn cache_one_hour_ttl_uses_anthropic_extended_cache_field() {
    let mut payload = base_payload();
    payload.cache = true;
    payload.prompt_cache_ttl = Some(crate::llm::api::PromptCacheTtl::OneHour);

    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["cache_control"],
        serde_json::json!({"type": "ephemeral", "ttl": "1h"})
    );
}
