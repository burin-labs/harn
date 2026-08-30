//! Non-streaming LLM response parsing. Covers Anthropic's `content`-array
//! shape and the OpenAI-compatible `choices[0].message` shape; streaming
//! variants live in [`super::transport`].

use crate::llm::usage::ProviderUsageReceipt;
use crate::value::{VmError, VmValue};

use super::result::{LlmResult, RawProviderToolCall};
use super::telemetry::ProviderTelemetry;

#[cfg(test)]
mod cache_mapping_tests;
#[cfg(test)]
mod test_support;

mod boundary;
mod cache_mapping;
mod completion_contract;
mod item_kinds;
mod openai;
pub(crate) use cache_mapping::{extract_cache_read_tokens, extract_cache_write_tokens};
pub(crate) use completion_contract::{
    billed_noncommittal_completion_error, empty_generation_error,
    is_billed_noncommittal_completion, is_length_stop_reason, openai_message_content_block_types,
    openai_reasoning_field_present, openai_responses_content_block_types,
    provider_content_block_types, CompletionContractSignals, ProviderResponseEnvelope,
};
pub(crate) use openai::parse_openai_responses_response;

#[cfg(test)]
#[path = "response_gateway_tests.rs"]
mod gateway_tests;

#[cfg(test)]
#[path = "response_signed_reasoning_tests.rs"]
mod signed_reasoning_tests;

#[cfg(test)]
#[path = "response/empty_generation_envelope_tests.rs"]
mod empty_generation_envelope_tests;

/// Parse a complete (non-streaming) LLM JSON response into an `LlmResult`.
pub(crate) fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    dialect: crate::llm::capabilities::WireDialect,
    tools_offered: bool,
) -> Result<LlmResult, VmError> {
    if provider == "openai"
        && json
            .get("output")
            .and_then(|value| value.as_array())
            .is_some()
    {
        return parse_openai_responses_response(json, provider, model);
    }

    if dialect == crate::llm::capabilities::WireDialect::Anthropic {
        if let Some(err) = json["error"]["message"].as_str() {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("{provider} API error: {err}"),
            ))));
        }

        let mut text = String::new();
        let mut thinking_text = String::new();
        let mut tool_calls = Vec::new();
        let mut raw_tool_calls = Vec::new();
        let mut blocks = Vec::new();

        let content = json
            .get("content")
            .and_then(|value| value.as_array())
            .ok_or_else(|| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "{provider} API response missing content array"
                ))))
            })?;
        for block in content {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        text.push_str(t);
                        blocks.push(serde_json::json!({"type": "output_text", "text": t, "visibility": "public"}));
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block["thinking"].as_str() {
                        thinking_text.push_str(t);
                    }
                    blocks.push(
                        crate::llm::reasoning_history::capture_anthropic_block(block)
                            .unwrap_or_else(|| {
                                serde_json::json!({
                                    "type": "reasoning",
                                    "text": block["thinking"].as_str().unwrap_or_default(),
                                    "visibility": "private"
                                })
                            }),
                    );
                }
                Some("redacted_thinking") => {
                    if let Some(block) =
                        crate::llm::reasoning_history::capture_anthropic_block(block)
                    {
                        blocks.push(block);
                    }
                }
                Some("tool_use") => {
                    raw_tool_calls.push(RawProviderToolCall::new(block.clone()).map_err(
                        |error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))),
                    )?);
                    let raw_name = block["name"].as_str().unwrap_or("").to_string();
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let input = block["input"].clone();
                    let (name, input) =
                        crate::llm::tools::normalize_tool_call_shape(&raw_name, input);
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "name": name,
                        "arguments": input,
                    }));
                    blocks.push(serde_json::json!({
                        "type": "tool_call",
                        "id": block["id"].clone(),
                        "name": name,
                        "arguments": input,
                        "visibility": "internal",
                    }));
                }
                Some("server_tool_use") => {
                    // Anthropic's server-side tool-search tool emits
                    // a `server_tool_use` content block when it
                    // queries. The model never sees this as a
                    // dispatchable tool — Anthropic executes it for
                    // us — so we record it for transcript/replay
                    // fidelity but do NOT add it to `tool_calls`.
                    blocks.push(serde_json::json!({
                        "type": "tool_search_query",
                        "id": block["id"].clone(),
                        "name": block["name"].clone(),
                        "query": block["input"].clone(),
                        "visibility": "internal",
                    }));
                }
                Some("tool_search_tool_result") => {
                    // Server-side search results. Anthropic
                    // auto-expands the referenced tools inline on
                    // subsequent turns; we just record the event so
                    // replay/eval can see which tools were promoted
                    // and when.
                    let references: Vec<serde_json::Value> = block["content"]["tool_references"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    blocks.push(serde_json::json!({
                        "type": "tool_search_result",
                        "tool_use_id": block["tool_use_id"].clone(),
                        "tool_references": references,
                        "visibility": "internal",
                    }));
                }
                other => boundary::unhandled_content_block(provider, other, block),
            }
        }

        let reported_input_tokens = json["usage"]["input_tokens"].as_i64();
        let reported_output_tokens = json["usage"]["output_tokens"].as_i64();
        let input_tokens = reported_input_tokens.unwrap_or(0);
        let output_tokens = reported_output_tokens.unwrap_or(0);
        let cache_read_tokens = extract_cache_read_tokens(&json["usage"]);
        let cache_write_tokens = extract_cache_write_tokens(&json["usage"]);
        let stop_reason = json["stop_reason"].as_str().map(|s| s.to_string());
        let request_id = json["id"].as_str().filter(|value| !value.is_empty());
        let telemetry = ProviderTelemetry::from_anthropic_usage(&json["usage"], request_id);
        let provider_usage = ProviderUsageReceipt::new(
            reported_input_tokens,
            reported_output_tokens,
            telemetry.provider_cost_usd,
            crate::llm::serving_tiers::served_fast(model, json),
        )
        .with_cache(
            cache_read_tokens,
            cache_write_tokens,
            telemetry.cache_accounting_declared,
            true,
        );

        if text.is_empty() && thinking_text.is_empty() && tool_calls.is_empty() && blocks.is_empty()
        {
            return Err(empty_generation_error(
                provider,
                model,
                ProviderResponseEnvelope::new(
                    request_id,
                    stop_reason.as_deref(),
                    provider_content_block_types(Some(content.as_slice())),
                    provider_usage,
                ),
                format!(
                    "anthropic-style model {model} delivered no content, reasoning, or tool calls"
                ),
            ));
        }

        Ok(LlmResult {
            attempts: Default::default(),
            text_projection: None,
            text,
            tool_calls,
            raw_tool_calls,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cache_supported: true,
            model: model.to_string(),
            provider: provider.to_string(),
            thinking: if thinking_text.is_empty() {
                None
            } else {
                Some(thinking_text)
            },
            thinking_summary: None,
            stop_reason,
            served_fast: crate::llm::serving_tiers::served_fast(model, json),
            blocks,
            logprobs: Vec::new(),
            telemetry,
        })
    } else {
        openai::parse_chat_completions_response(json, provider, model, tools_offered)
    }
}

#[cfg(test)]
mod raw_tool_receipts_tests;

#[cfg(test)]
mod tests {
    use crate::llm::usage::ProviderUsageReceipt;
    use crate::value::{VmError, VmValue};

    use super::super::openai_normalize::{
        extract_openai_choice_logprobs, parse_tool_arguments, preview_chars,
    };
    use super::{
        extract_cache_read_tokens, extract_cache_write_tokens, is_billed_noncommittal_completion,
        parse_openai_responses_response, test_support::parse_llm_response,
        CompletionContractSignals,
    };

    fn assert_provider_usage_receipt(error: &VmError, input_tokens: i64, output_tokens: i64) {
        let receipt = ProviderUsageReceipt::from_error(error)
            .expect("parser error must retain a typed provider usage receipt");
        let VmValue::Dict(fields) = receipt.to_vm_value() else {
            panic!("receipt must lower to a dictionary");
        };
        assert_eq!(
            fields.get("input_tokens").and_then(VmValue::as_int),
            Some(input_tokens)
        );
        assert_eq!(
            fields.get("output_tokens").and_then(VmValue::as_int),
            Some(output_tokens)
        );
    }

    #[test]
    fn parse_tool_arguments_preview_does_not_panic_mid_utf8() {
        // The old 200-byte slice landed inside the multibyte character and
        // panicked while building the invalid-JSON preview.
        let mut malformed = "a".repeat(199);
        malformed.push('→'); // 3 bytes (E2 86 92), straddles byte 200
        malformed.push_str(" not json");
        let value = parse_tool_arguments(Some(&serde_json::Value::String(malformed.clone())));
        let preview = value["__parse_error"]
            .as_str()
            .expect("parse error preview");
        assert!(preview.contains("Could not parse tool arguments"));
    }

    #[test]
    fn preview_chars_is_char_boundary_safe_and_caps_chars() {
        let s = format!("{}é", "x".repeat(199)); // 199 ASCII + 2-byte char
        let out = preview_chars(&s, 200);
        assert_eq!(out.chars().count(), 200);
        // Multibyte content survives intact without a panic.
        assert!(preview_chars("→→→", 1).chars().count() <= 1);
    }

    #[test]
    fn contract_violation_fires_on_billed_noop_tool_turn() {
        // Hidden-action shape: clean `stop`, billed tokens, tools offered, no
        // visible text, no tool call, no tool-search.
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 342,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_normal_tool_call() {
        let signals = CompletionContractSignals {
            stop_reason: Some("tool_calls"),
            output_tokens: 800,
            tools_offered: true,
            tool_call_count: 2,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_committed_text_answer() {
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 120,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "pong:catalog-refresh",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_truncation() {
        // A `length`/truncation finish must never misfire even when short.
        let signals = CompletionContractSignals {
            stop_reason: Some("length"),
            output_tokens: 4096,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_max_tokens_truncation() {
        let signals = CompletionContractSignals {
            stop_reason: Some("max_tokens"),
            output_tokens: 4096,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_when_no_tools_offered() {
        // A deliberately terse text reply to a tool-less prompt is fine.
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 6,
            tools_offered: false,
            tool_call_count: 0,
            has_tool_search_block: false,
            text: "Yes.",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn contract_violation_silent_on_tool_search_block() {
        let signals = CompletionContractSignals {
            stop_reason: Some("stop"),
            output_tokens: 200,
            tools_offered: true,
            tool_call_count: 0,
            has_tool_search_block: true,
            text: "",
        };
        assert!(!is_billed_noncommittal_completion(&signals));
    }

    #[test]
    fn parse_llm_response_rejects_ambient_billed_noop() {
        // End-to-end through the openai-compat parser: clean stop, billed
        // hidden reasoning, no visible content, and empty tool_calls must
        // surface a loud contract-violation error rather than a silent empty
        // success.
        let response = serde_json::json!({
            "id": "gen-ambient",
            "model": "qwen/qwen3.6-35b-a3b",
            "provider": "Ambient",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "creating files"
                }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 342 }
        });
        let err = parse_llm_response(&response, "openrouter", "qwen/qwen3.6-35b-a3b", false, true)
            .expect_err("billed no-op tool turn must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("upstream contract violation"),
            "error must name the contract violation: {message}"
        );
        assert_provider_usage_receipt(&err, 321, 342);
    }

    #[test]
    fn parse_llm_response_rejects_structured_reasoning_only_tool_turn() {
        // Same contract as top-level `reasoning_content`, but through
        // structured content blocks. A pseudo-call in private reasoning is not
        // committed visible text and must never be accepted as a tool turn.
        let response = serde_json::json!({
            "id": "gen-structured-hidden",
            "model": "qwen/qwen3.6-35b-a3b",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "reasoning",
                        "text": "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>"
                    }]
                }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 342 }
        });

        let err = parse_llm_response(&response, "openrouter", "qwen/qwen3.6-35b-a3b", false, true)
            .expect_err("structured hidden reasoning must not satisfy a tool turn");

        let message = err.to_string();
        assert!(
            message.contains("upstream contract violation"),
            "error must name the contract violation: {message}"
        );
    }

    #[test]
    fn parse_llm_response_allows_billed_empty_length_truncation() {
        // Some reasoning routes consume the output cap in a hidden channel and
        // return no visible content or reasoning string, only a length stop and
        // billed completion tokens. The parser must hand this shape to the
        // agent loop so it can auto-continue with a raised cap.
        let response = serde_json::json!({
            "id": "gen-hidden-truncated",
            "choices": [{
                "index": 0,
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": ""
                }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 342 }
        });
        let result = parse_llm_response(&response, "openrouter", "hidden-reasoning", false, true)
            .expect("billed length truncation is recoverable");

        assert_eq!(result.text, "");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.output_tokens, 342);
        assert_eq!(result.stop_reason.as_deref(), Some("length"));
        assert_eq!(result.thinking, None);
        assert_eq!(result.thinking_summary, None);
    }

    #[test]
    fn parse_llm_response_allows_short_answer_when_no_tools_offered() {
        // Same short content, but no tools were offered: this is a legitimate
        // terse answer and must parse cleanly.
        let response = serde_json::json!({
            "id": "gen-terse",
            "model": "qwen/qwen3.6-35b-a3b",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "creating files\n\n" }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 6 }
        });
        let result = parse_llm_response(
            &response,
            "openrouter",
            "qwen/qwen3.6-35b-a3b",
            false,
            false,
        )
        .expect("short answer with no tools offered parses cleanly");
        assert_eq!(result.text.trim(), "creating files");
    }

    #[test]
    fn parse_llm_response_allows_short_committed_answer_when_tools_were_offered() {
        // Agent loops often request a terse final token after a tool result.
        // The parser must not guess that a short visible answer is a no-op.
        let response = serde_json::json!({
            "id": "gen-terse-tool-answer",
            "model": "claude-haiku-4-5-20251001",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "pong:catalog-refresh" }
            }],
            "usage": { "prompt_tokens": 321, "completion_tokens": 9 }
        });
        let result = parse_llm_response(
            &response,
            "anthropic",
            "claude-haiku-4-5-20251001",
            false,
            true,
        )
        .expect("short committed answer with tools offered parses cleanly");
        assert_eq!(result.text.trim(), "pong:catalog-refresh");
    }

    #[test]
    fn parse_llm_response_keeps_reasoning_only_clean_stop_private_by_default() {
        // A route with no explicit capability row must not treat a provider
        // reasoning field as committed answer text. Known exceptions can opt in
        // with `reasoning_text_promotable = true`.
        let response = serde_json::json!({
            "id": "gen-reasoning-only",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "private answer-shaped trace"
                }
            }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 4 }
        });
        let result = parse_llm_response(&response, "openai", "synthetic-default", false, false)
            .expect("reasoning-only stop should parse without visible promotion");

        assert_eq!(result.text, "");
        assert_eq!(
            result.thinking.as_deref(),
            Some("private answer-shaped trace")
        );
    }

    #[test]
    fn cache_write_tokens_supports_openrouter_prompt_details_shape() {
        let usage = serde_json::json!({
            "prompt_tokens": 194,
            "completion_tokens": 2,
            "prompt_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 100
            }
        });

        assert_eq!(extract_cache_write_tokens(&usage), 100);
    }
    #[test]
    fn cache_tokens_support_openai_responses_details_shape() {
        let usage = serde_json::json!({
            "input_tokens": 194,
            "output_tokens": 2,
            "input_tokens_details": {
                "cached_tokens": 120,
                "cache_creation_input_tokens": 40
            }
        });

        assert_eq!(extract_cache_read_tokens(&usage), 120);
        assert_eq!(extract_cache_write_tokens(&usage), 40);
    }
    #[test]
    fn cache_tokens_support_deepseek_prompt_cache_hit_field() {
        // DeepSeek (and some OpenRouter passthrough shapes for it) reports
        // cache hits as `prompt_cache_hit_tokens` instead of the
        // Anthropic-style top-level or OpenAI-style nested field. Without
        // this row a real cache hit reads as 0 (harn#2320).
        let usage = serde_json::json!({
            "prompt_tokens": 9100,
            "completion_tokens": 42,
            "prompt_cache_hit_tokens": 8800
        });
        assert_eq!(extract_cache_read_tokens(&usage), 8800);
    }
    #[test]
    fn cache_tokens_support_openrouter_cache_subobject_shape() {
        // OpenRouter's newer 2026-04 "Caching → Anthropic" wire shape
        // surfaces cache attribution under a `cache` sub-object instead
        // of mirroring Anthropic's top-level fields verbatim.
        let usage = serde_json::json!({
            "prompt_tokens": 9100,
            "completion_tokens": 42,
            "cache": {
                "read_input_tokens": 8800,
                "write_input_tokens": 220
            }
        });
        assert_eq!(extract_cache_read_tokens(&usage), 8800);
        assert_eq!(extract_cache_write_tokens(&usage), 220);
    }

    #[test]
    fn parses_openai_responses_structured_output() {
        let json = serde_json::json!({
            "id": "resp_123",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg_123",
                "content": [{
                    "type": "output_text",
                    "text": "{\"ok\":true}"
                }]
            }],
            "usage": {
                "input_tokens": 9,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": 3}
            }
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert_eq!(result.text, "{\"ok\":true}");
        assert_eq!(result.input_tokens, 9);
        assert_eq!(result.output_tokens, 5);
        assert_eq!(result.cache_read_tokens, 3);
        assert_eq!(result.telemetry.request_id.as_deref(), Some("resp_123"));
        assert_eq!(result.blocks[0]["type"], "output_text");
    }

    #[test]
    fn parses_openai_responses_hosted_tool_metadata() {
        let json = serde_json::json!({
            "id": "resp_456",
            "status": "completed",
            "output": [{
                "type": "web_search_call",
                "id": "ws_123",
                "call_id": "call_ws_123",
                "status": "completed",
                "action": {"query": "Harn orchestration"}
            }],
            "usage": {"input_tokens": 4, "output_tokens": 1}
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert!(result.tool_calls.is_empty());
        let block = &result.blocks[0];
        assert_eq!(block["type"], "provider_tool_call");
        assert_eq!(block["provider_tool_id"], "ws_123");
        assert_eq!(block["call_id"], "call_ws_123");
        assert_eq!(block["provider_tool_type"], "web_search_call");
        assert_eq!(block["tool_kind"], "web_search");
        assert_eq!(block["executor"], "provider_native");
        assert_eq!(
            block["provider_metadata"]["action"]["query"],
            "Harn orchestration"
        );
    }

    #[test]
    fn parses_openai_responses_compaction_metadata() {
        let json = serde_json::json!({
            "id": "resp_compact",
            "status": "completed",
            "output": [{
                "type": "compaction",
                "id": "cmp_123",
                "encrypted_content": "opaque-state"
            }],
            "usage": {"input_tokens": 20, "output_tokens": 0}
        });

        let result =
            parse_openai_responses_response(&json, "openai", "gpt-5.4").expect("response parses");

        assert!(result.text.is_empty());
        let block = &result.blocks[0];
        assert_eq!(block["type"], "compaction");
        assert_eq!(block["provider_id"], "cmp_123");
        assert_eq!(block["encrypted_content"], "opaque-state");
        assert_eq!(block["visibility"], "private");
    }

    #[test]
    fn extracts_chat_completion_logprobs() {
        let choice = serde_json::json!({
            "logprobs": {
                "content": [
                    {
                        "token": "safe",
                        "logprob": -0.1,
                        "bytes": [115, 97, 102, 101],
                        "top_logprobs": [
                            {"token": "safe", "logprob": -0.1},
                            {"token": "risky", "logprob": -2.4}
                        ]
                    }
                ]
            }
        });

        let logprobs = extract_openai_choice_logprobs(&choice);

        assert_eq!(logprobs.len(), 1);
        assert_eq!(logprobs[0]["token"].as_str(), Some("safe"));
        assert_eq!(logprobs[0]["logprob"].as_f64(), Some(-0.1));
        let top = logprobs[0]["top_logprobs"]
            .as_array()
            .expect("top logprobs array");
        assert_eq!(top.len(), 2);
        assert_eq!(top[1]["token"].as_str(), Some("risky"));
    }

    #[test]
    fn extracts_legacy_completion_logprobs() {
        let choice = serde_json::json!({
            "logprobs": {
                "tokens": ["safe"],
                "token_logprobs": [-0.1],
                "top_logprobs": [
                    {"safe": -0.1, "risky": -2.4}
                ]
            }
        });

        let logprobs = extract_openai_choice_logprobs(&choice);

        assert_eq!(logprobs.len(), 1);
        assert_eq!(logprobs[0]["token"].as_str(), Some("safe"));
        assert_eq!(logprobs[0]["logprob"].as_f64(), Some(-0.1));
        let top = logprobs[0]["top_logprobs"]
            .as_array()
            .expect("top logprobs array");
        assert_eq!(top.len(), 2);
        assert!(top.iter().any(|item| {
            item.get("token").and_then(|value| value.as_str()) == Some("risky")
                && item.get("logprob").and_then(|value| value.as_f64()) == Some(-2.4)
        }));
    }

    #[test]
    fn anthropic_parser_rejects_missing_content_array() {
        let response = serde_json::json!({
            "id": "msg_bad",
            "usage": {"input_tokens": 1, "output_tokens": 0}
        });

        let error = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect_err("missing content must be rejected");

        assert!(error.to_string().contains("missing content array"));
    }

    #[test]
    fn openai_parser_rejects_missing_choices_array() {
        let response = serde_json::json!({
            "id": "chatcmpl-bad",
            "usage": {"prompt_tokens": 1, "completion_tokens": 0}
        });

        let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect_err("missing choices must be rejected");

        assert!(error
            .to_string()
            .contains("missing non-empty choices array"));
    }

    #[test]
    fn anthropic_parser_records_server_tool_use_as_tool_search_query() {
        // Build a minimal Anthropic Messages API response containing a
        // server_tool_use block (the model calling the search tool).
        let response = serde_json::json!({
            "content": [
                {"type": "text", "text": "searching now"},
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_01",
                    "name": "tool_search_tool_bm25",
                    "input": {"query": "weather"}
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect("parser succeeds");

        // tool_calls is for *dispatchable* user tools — server-side tools
        // must never appear there.
        assert!(result.tool_calls.is_empty());

        // The tool_search_query event is on the blocks list.
        let has_query_event = result.blocks.iter().any(|b| {
            b.get("type").and_then(|v| v.as_str()) == Some("tool_search_query")
                && b.get("name").and_then(|v| v.as_str()) == Some("tool_search_tool_bm25")
        });
        assert!(
            has_query_event,
            "expected tool_search_query block; got {:#?}",
            result.blocks
        );
    }

    #[test]
    fn openai_parser_keeps_finish_reason_on_empty_args_tool_call() {
        // IDE-host bug-report evidence shape (non-streaming): the provider
        // boundary delivers a named tool call with literally "{}" arguments.
        // `finish_reason` must surface as `stop_reason` so downstream
        // feedback can distinguish a length-truncated call from a clean-stop
        // provider drop.
        for finish_reason in ["length", "tool_calls"] {
            let response = serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "tool_calls": [
                            {
                                "id": "chatcmpl-tool-1",
                                "type": "function",
                                "function": {"name": "edit", "arguments": "{}"}
                            }
                        ]
                    },
                    "finish_reason": finish_reason
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 549}
            });
            let result = parse_llm_response(&response, "openrouter", "or-qwen", false, false)
                .expect("parser succeeds");
            assert_eq!(result.stop_reason.as_deref(), Some(finish_reason));
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0]["name"], "edit");
            assert_eq!(result.tool_calls[0]["arguments"], serde_json::json!({}));
        }
    }

    #[test]
    fn openai_parser_recovers_text_tool_call_misplaced_into_name() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_look_text_name",
                            "type": "function",
                            "function": {
                                "name": "look({ file: \"include/kvdb/status.h\", intent: \"read\" })</arg_value>",
                                "arguments": "{}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result =
            parse_llm_response(&response, "zai", "glm-5", false, false).expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["id"], "call_look_text_name");
        assert_eq!(result.tool_calls[0]["name"], "look");
        assert_eq!(
            result.tool_calls[0]["arguments"]["file"],
            "include/kvdb/status.h"
        );
        assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    }

    #[test]
    fn openai_parser_recovers_text_tool_arguments_in_native_arguments() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_look_text_arguments",
                            "type": "function",
                            "function": {
                                "name": "look",
                                "arguments": "{ file: \"include/kvdb/status.h\", intent: \"read\" }"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result =
            parse_llm_response(&response, "zai", "glm-5", false, false).expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["id"], "call_look_text_arguments");
        assert_eq!(result.tool_calls[0]["name"], "look");
        assert_eq!(
            result.tool_calls[0]["arguments"]["file"],
            "include/kvdb/status.h"
        );
        assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    }

    #[test]
    fn openai_parser_recovers_text_tool_call_wrapped_in_native_tool_call_arguments() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_wrapper_args",
                            "type": "function",
                            "function": {
                                "name": "tool_call",
                                "arguments": "<tool_call>\nlook({ file: \"Sources/App.swift\", intent: \"read\" })\n</tool_call>"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result = parse_llm_response(&response, "openrouter", "gpt-oss-120b", false, false)
            .expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["id"], "call_wrapper_args");
        assert_eq!(result.tool_calls[0]["name"], "look");
        assert_eq!(
            result.tool_calls[0]["arguments"]["file"],
            "Sources/App.swift"
        );
        assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    }

    #[test]
    fn openai_parser_recovers_text_tool_call_with_malformed_wrapper_suffix() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_wrapper_args",
                            "type": "function",
                            "function": {
                                "name": "tool_call",
                                "arguments": "<tool_call>\nlook({ file: \"app/Enums/FieldType.php\", intent: \"read\" })\n</tool_call<|message|>"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result = parse_llm_response(&response, "fireworks", "gpt-oss-120b", false, false)
            .expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["id"], "call_wrapper_args");
        assert_eq!(result.tool_calls[0]["name"], "look");
        assert_eq!(
            result.tool_calls[0]["arguments"]["file"],
            "app/Enums/FieldType.php"
        );
        assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    }

    #[test]
    fn openai_parser_rejects_partial_text_tool_call_wrapped_in_native_tool_call_arguments() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_wrapper_partial_args",
                            "type": "function",
                            "function": {
                                "name": "tool_call",
                                "arguments": "<tool_call>\nedit({ action: \"create\", path: \"tests/page_cache_extra_test.cpp\", content: <<EOF"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result = parse_llm_response(&response, "openrouter", "gpt-oss-120b", false, false)
            .expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0]["name"], "edit",
            "partial nested text call must not be dispatched as literal tool_call"
        );
        let parse_error = result.tool_calls[0]["arguments"]["__parse_error"]
            .as_str()
            .expect("partial nested text call should carry a parse error");
        assert!(parse_error.contains("provider tool arguments"));
        assert!(parse_error.contains("Raw input: <tool_call>"));
    }

    #[test]
    fn openai_parser_rejects_partial_text_tool_call_misplaced_into_name() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_edit_partial_name",
                            "type": "function",
                            "function": {
                                "name": "edit({ action: \"create\", path: \"tests/page_cache_extra_test.cpp\", content: <<EOF",
                                "arguments": "{"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 30}
        });

        let result =
            parse_llm_response(&response, "zai", "glm-5", false, false).expect("parser succeeds");

        assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0]["name"], "edit",
            "partial text-call name must not be dispatched as a bogus native name"
        );
        let parse_error = result.tool_calls[0]["arguments"]["__parse_error"]
            .as_str()
            .expect("partial text-call name should carry a parse error");
        assert!(parse_error.contains("provider tool name"));
        assert!(parse_error.contains("Raw input: edit({ action"));
    }

    #[test]
    fn openai_parser_splits_concatenated_tool_argument_objects() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "chatcmpl-tool-1",
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"a.rs\"}{\"path\":\"b.rs\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(&response, "openrouter", "google/gemma-4", false, false)
            .expect("parser succeeds");

        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0]["id"], "chatcmpl-tool-1");
        assert_eq!(result.tool_calls[0]["name"], "read");
        assert_eq!(result.tool_calls[0]["arguments"]["path"], "a.rs");
        assert_eq!(result.tool_calls[1]["id"], "chatcmpl-tool-1_2");
        assert_eq!(result.tool_calls[1]["name"], "read");
        assert_eq!(result.tool_calls[1]["arguments"]["path"], "b.rs");
        let tool_blocks = result
            .blocks
            .iter()
            .filter(|block| block["type"] == "tool_call")
            .collect::<Vec<_>>();
        assert_eq!(tool_blocks.len(), 2);
        assert_eq!(tool_blocks[1]["id"], "chatcmpl-tool-1_2");
    }

    #[test]
    fn openai_parser_splits_concatenated_tool_arguments_without_source_id() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "type": "function",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"a.rs\"}{\"path\":\"b.rs\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(&response, "openrouter", "google/gemma-4", false, false)
            .expect("parser succeeds");

        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0]["id"], "call_0_1");
        assert_eq!(result.tool_calls[1]["id"], "call_0_2");
    }

    #[test]
    fn openai_parser_normalizes_canonical_run_argv_command() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_run_argv",
                            "type": "function",
                            "function": {
                                "name": "run",
                                "arguments": "{\"command\":[\"bash\",\"lc\",\"ls -R\"]}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        });

        let result = parse_llm_response(
            &response,
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            false,
            false,
        )
        .expect("parser succeeds");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0]["name"], "run");
        assert_eq!(result.tool_calls[0]["arguments"]["command"], "ls -R");
    }

    #[test]
    fn openai_parser_records_tool_search_call_as_query_event() {
        // OpenAI's Responses API (harn#71) surfaces the server-hosted
        // tool_search as a `tool_search_call` entry in the `tool_calls`
        // array. The parser must NOT add it to the dispatchable
        // `tool_calls` vector — OpenAI runs the search on their side —
        // but must record a `tool_search_query` transcript block so
        // replay lines up with the Anthropic path.
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "searching",
                    "tool_calls": [
                        {
                            "id": "tsc_01",
                            "type": "tool_search_call",
                            "query": {"q": "weather"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect("parser succeeds");

        assert!(
            result.tool_calls.is_empty(),
            "tool_search_call is server-executed; must not be dispatchable"
        );
        let query = result
            .blocks
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_query"))
            .expect("tool_search_query block present");
        assert_eq!(query["id"].as_str(), Some("tsc_01"));
        assert_eq!(query["query"]["q"].as_str(), Some("weather"));
    }

    #[test]
    fn openai_parser_records_tool_search_output_as_result_event() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "tso_01",
                            "type": "tool_search_output",
                            "call_id": "tsc_01",
                            "tool_references": [
                                {"tool_name": "get_weather"}
                            ]
                        }
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        });
        let result = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
            .expect("parser succeeds");

        assert!(result.tool_calls.is_empty());
        let result_block = result
            .blocks
            .iter()
            .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_result"))
            .expect("tool_search_result block present");
        assert_eq!(result_block["tool_use_id"].as_str(), Some("tsc_01"));
        let refs = result_block["tool_references"]
            .as_array()
            .expect("tool_references array");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["tool_name"].as_str(), Some("get_weather"));
    }

    #[test]
    fn openai_parser_surfaces_reasoning_summary_separate_from_text() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Final answer.",
                    "reasoning_summary": [
                        {"type": "summary_text", "text": "Checked the constraints."},
                        {"type": "summary_text", "text": "Chose the direct answer."}
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7}
        });

        let result =
            parse_llm_response(&response, "openai", "o3", false, false).expect("parser succeeds");

        assert_eq!(result.text, "Final answer.");
        assert_eq!(
            result.thinking_summary.as_deref(),
            Some("Checked the constraints.\nChose the direct answer.")
        );
        assert_eq!(result.thinking, None);
        assert!(result.blocks.iter().any(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("reasoning_summary")
                && block.get("text").and_then(|value| value.as_str())
                    == Some("Checked the constraints.\nChose the direct answer.")
        }));
    }

    #[test]
    fn openai_parser_lifts_llamacpp_timings_into_telemetry() {
        // Captured from llama-server b10603-c060ca974. Its timing breakdown is
        // a response-root sibling of `usage`, and a warm prompt cache makes
        // the full prompt count differ sharply from the work the server did.
        let response = serde_json::json!({
            "choices": [{
                "message": {"content": "answer"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 6036, "completion_tokens": 17},
            "timings": {
                "prompt_n": 4,
                "cache_n": 6032,
                "prompt_ms": 145.4,
                "predicted_n": 17,
                "predicted_ms": 89.1
            }
        });

        let result = parse_llm_response(&response, "llamacpp", "qwen-7b", false, false)
            .expect("parser succeeds");

        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::LLAMACPP_TIMINGS
        );
        assert_eq!(result.telemetry.server_prompt_eval_ms, Some(145));
        assert_eq!(result.telemetry.server_generation_ms, Some(89));
        assert_eq!(result.telemetry.server_total_ms, Some(234));
        assert_eq!(result.telemetry.server_prompt_tokens, Some(6036));
        assert_eq!(result.telemetry.server_uncached_prompt_tokens, Some(4));
        assert_eq!(result.telemetry.server_cached_prompt_tokens, Some(6032));
    }

    #[test]
    fn anthropic_parser_captures_request_id_in_telemetry() {
        let response = serde_json::json!({
            "id": "msg_01ABC",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 5, "output_tokens": 2},
            "stop_reason": "end_turn"
        });
        let result = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
            .expect("parser succeeds");
        assert_eq!(
            result.telemetry.source,
            crate::llm::api::telemetry_source::ANTHROPIC_USAGE
        );
        assert_eq!(result.telemetry.request_id.as_deref(), Some("msg_01ABC"));
        assert_eq!(result.telemetry.server_prompt_tokens, Some(5));
        assert_eq!(result.telemetry.server_output_tokens, Some(2));
    }
}
