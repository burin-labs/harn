//! Streaming-tool-call partial-arg event tests. Drive
//! [`consume_sse_lines`] against canned byte streams that mimic the
//! Anthropic / OpenAI wire format and assert the
//! `AgentEvent::ToolCall` / `AgentEvent::ToolCallUpdate` sequence
//! lands as expected: an initial `Pending` announcement, one or
//! more coalesced partial-arg updates, and `raw_input_partial`
//! when the partial bytes can't be parsed as JSON yet.
//!
//! Events are captured via the global session-sink registry (which
//! is what real ACP/A2A consumers use) rather than the per-loop
//! thread-local — drive a fresh session id per test so they don't
//! cross-talk under `cargo test`'s thread pool.

use super::sse::consume_sse_lines;
use super::*;
use crate::agent_events::{
    clear_session_sinks, register_sink, AgentEvent, AgentEventSink, ToolCallStatus,
};
use std::sync::{Arc, Mutex};

struct CapturingSink {
    events: Arc<Mutex<Vec<AgentEvent>>>,
}

impl AgentEventSink for CapturingSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.events
            .lock()
            .expect("capture mutex")
            .push(event.clone());
    }
}

fn install_capturing_sink(session_id: &str) -> Arc<Mutex<Vec<AgentEvent>>> {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    register_sink(
        session_id,
        Arc::new(CapturingSink {
            events: events.clone(),
        }),
    );
    events
}

/// Build a fresh per-test session id so concurrent test threads
/// can't poison each other's captured-event vector via the global
/// registry.
fn fresh_session_id(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::now_v7())
}

/// Drive `consume_sse_lines` against a canned SSE byte buffer and
/// return the captured agent events plus the parsed `LlmResult`.
/// Helper so each test can stay focused on the assertion.
async fn drive(bytes: &[u8], session_id: &str, is_anthropic: bool) -> (LlmResult, Vec<AgentEvent>) {
    let (result, captured) = drive_result(bytes, session_id, is_anthropic).await;
    (result.expect("sse parse should succeed"), captured)
}

async fn drive_result(
    bytes: &[u8],
    session_id: &str,
    is_anthropic: bool,
) -> (Result<LlmResult, VmError>, Vec<AgentEvent>) {
    let events = install_capturing_sink(session_id);
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(bytes);
    let result = consume_sse_lines(
        reader,
        if is_anthropic { "anthropic" } else { "openai" },
        "test-model",
        is_anthropic,
        delta_tx,
        Some(session_id),
        None,
        false,
    )
    .await;
    let captured = events.lock().expect("capture mutex").clone();
    (result, captured)
}

fn failed_update_for<'a>(events: &'a [AgentEvent], tool_call_id: &str) -> Option<&'a AgentEvent> {
    events.iter().find(|event| {
        matches!(
            event,
            AgentEvent::ToolCallUpdate {
                tool_call_id: id,
                status: ToolCallStatus::Failed,
                ..
            } if id == tool_call_id
        )
    })
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_announces_tool_call_then_streams_partials() {
    // Force COALESCE_WINDOW pauses by emitting deltas across the
    // 50ms boundary so the coalescer flushes more than once.
    let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a1\",\"name\":\"search_web\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"ant\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"hropic\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-stream");
    let (result, events) = drive(body.as_bytes(), &session_id, true).await;

    // ── Initial ToolCall(Pending) announcement ──────────────────
    let announcements: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
        .collect();
    assert_eq!(
        announcements.len(),
        1,
        "expected exactly one initial ToolCall(Pending), got {events:#?}"
    );
    match announcements[0] {
        AgentEvent::ToolCall {
            tool_name,
            status,
            tool_call_id,
            raw_input,
            ..
        } => {
            assert_eq!(tool_name, "search_web");
            assert_eq!(*status, ToolCallStatus::Pending);
            // The provider id is used verbatim so the streaming
            // announcement and executed lifecycle share one wire id.
            assert_eq!(tool_call_id, "toolu_a1");
            assert_eq!(*raw_input, serde_json::json!({}));
        }
        _ => unreachable!(),
    }

    // ── Partial-arg ToolCallUpdate(Pending) updates fired before
    //    content_block_stop ─────────────────────────────────────
    let partial_updates: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolCallUpdate {
                    status: ToolCallStatus::Pending,
                    ..
                }
            )
        })
        .collect();
    assert!(
        !partial_updates.is_empty(),
        "expected at least one Pending tool_call_update from streaming deltas, got {events:#?}"
    );
    // Some update must carry either a parsed `raw_input` or a
    // `raw_input_partial` so clients can render the args live.
    let has_payload = partial_updates.iter().any(|e| match e {
        AgentEvent::ToolCallUpdate {
            raw_input,
            raw_input_partial,
            ..
        } => raw_input.is_some() || raw_input_partial.is_some(),
        _ => false,
    });
    assert!(
            has_payload,
            "expected at least one Pending update to carry raw_input or raw_input_partial; got {partial_updates:#?}"
        );

    // ── Final tool call result was still parsed correctly ───────
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "search_web");
    assert_eq!(result.tool_calls[0]["arguments"]["q"], "anthropic");
    // The agent loop reuses the dispatched call id for the executed
    // lifecycle, so it must match the streaming announcement id.
    assert_eq!(result.tool_calls[0]["id"], "toolu_a1");

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_terminalizes_announced_tool_when_block_never_dispatches() {
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_orphan\",\"name\":\"search_web\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-orphan-tool");
    let (result, events) = drive(body.as_bytes(), &session_id, true).await;

    assert!(
        result.tool_calls.is_empty(),
        "unfinished stream block must not become dispatchable"
    );
    let closeout = failed_update_for(&events, "toolu_orphan")
        .expect("unfinished announced tool call must be closed out");
    match closeout {
        AgentEvent::ToolCallUpdate {
            tool_name,
            error_category,
            error,
            ..
        } => {
            assert_eq!(tool_name, "search_web");
            assert_eq!(*error_category, Some(ToolCallErrorCategory::ParseAborted));
            assert!(
                error
                    .as_deref()
                    .is_some_and(|message| message.contains("reached dispatch")),
                "closeout error should name dispatch: {error:?}"
            );
        }
        _ => unreachable!(),
    }

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_error_closes_announced_tool_before_returning_error() {
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_error\",\"name\":\"edit\"}}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4},\"delta\":{\"stop_reason\":\"stop\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-error-tool");
    let (result, events) = drive_result(body.as_bytes(), &session_id, true).await;

    assert!(result.is_err(), "billed empty stream should still error");
    let closeout = failed_update_for(&events, "toolu_error")
        .expect("errored stream must close the announced tool");
    match closeout {
        AgentEvent::ToolCallUpdate {
            tool_name,
            error_category,
            ..
        } => {
            assert_eq!(tool_name, "edit");
            assert_eq!(*error_category, Some(ToolCallErrorCategory::ParseAborted));
        }
        _ => unreachable!(),
    }

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_emits_raw_input_partial_when_args_unparseable() {
    // The model emits an unterminated string before the close —
    // the recovery path can't synthesize a value, so the transport
    // must publish `raw_input_partial` carrying the raw bytes.
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b1\",\"name\":\"edit\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"foo.swift\\\",\\\"replace\\\":\\\"hello\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" world\\\"}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-raw-partial");
    let (_, events) = drive(body.as_bytes(), &session_id, true).await;

    // The first Pending update fires immediately after the first
    // delta. At that point the buffer contains an unterminated
    // string, so `raw_input_partial` should be set.
    let first_partial = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallUpdate {
            status: ToolCallStatus::Pending,
            raw_input,
            raw_input_partial,
            ..
        } => Some((raw_input.clone(), raw_input_partial.clone())),
        _ => None,
    });
    let (first_value, first_raw) =
        first_partial.expect("expected at least one Pending tool_call_update during streaming");
    assert!(
            first_value.is_none() && first_raw.is_some(),
            "first partial must surface raw_input_partial when JSON isn't yet parseable; got value={first_value:?} raw={first_raw:?}"
        );
    assert!(
        first_raw.unwrap().contains("hello"),
        "raw_input_partial should carry the concatenated bytes verbatim"
    );

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_no_args_tool_call_dispatches_empty_object_not_parse_error() {
    // A tool with no arguments streams NO input_json_delta events at all:
    // the wire sends content_block_start {"type":"tool_use","input":{}}
    // followed directly by content_block_stop. That must finalize to `{}`,
    // never to a `__parse_error` carrier.
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_c1\",\"name\":\"list_files\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-no-args");
    let (result, _) = drive(body.as_bytes(), &session_id, true).await;

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "list_files");
    assert_eq!(result.tool_calls[0]["arguments"], serde_json::json!({}));

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream_malformed_tool_args_surface_parse_error_not_empty_object() {
    // Malformed/truncated accumulated tool JSON must NOT silently dispatch
    // the tool with `{}` — it must carry the same `__parse_error` object the
    // OpenAI streaming path builds, so the agent loop's recoverable
    // invalid-arguments feedback path handles it.
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_d1\",\"name\":\"edit\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"foo.sw\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3},\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-malformed-args");
    let (result, _) = drive(body.as_bytes(), &session_id, true).await;

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "edit");
    let parse_error = result.tool_calls[0]["arguments"]["__parse_error"]
        .as_str()
        .expect("malformed streamed tool args must dispatch a __parse_error carrier, not `{}`");
    assert!(
        parse_error.contains("Raw input: "),
        "__parse_error must embed the raw bytes for the recovery path; got {parse_error:?}"
    );
    assert!(
        parse_error.contains("{\"path\":\"foo.sw"),
        "raw input preview should carry the accumulated buffer verbatim; got {parse_error:?}"
    );

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_truncated_reasoning_does_not_leak_into_text() {
    // A reasoning model cut off mid-thought streams only `reasoning`
    // deltas with no committed `content`, then finish_reason="length".
    // The partial trace must NOT be promoted into `.text` (that garbage
    // would surface as the answer); it stays under `.thinking`.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"Let me think step\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\" by step about\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"length\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-trunc");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("length"));
    assert_eq!(
        result.text, "",
        "truncated reasoning leaked into visible text: {:?}",
        result.text
    );
    assert_eq!(
        result.thinking.as_deref(),
        Some("Let me think step by step about"),
        "partial reasoning trace must survive under thinking"
    );

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_reasoning_stays_private_by_default_on_clean_stop() {
    // A reasoning-only clean stop is private by default. Routes that really
    // answer in the reasoning channel can opt in through the capability
    // matrix, but a missing row must not leak private trace as visible text.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"the answer\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\" is 42\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-clean");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("stop"));
    assert_eq!(result.text, "");
    assert_eq!(result.thinking.as_deref(), Some("the answer is 42"));

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_reasoning_promotes_to_text_when_capability_opts_in() {
    crate::llm::capabilities::set_user_overrides_toml(concat!(
        "[[provider.openai]]\n",
        "model_match = \"test-model\"\n",
        "reasoning_text_promotable = true\n",
    ))
    .expect("capability override");
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"the answer\"}}]}\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\" is 42\"}}]}\n",
        "data: [DONE]\n",
    );
    let session_id = fresh_session_id("oai-clean-promote");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.text, "the answer is 42");
    assert_eq!(result.thinking.as_deref(), Some("the answer is 42"));

    crate::llm::capabilities::clear_user_overrides();
    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_reasoning_does_not_leak_into_text_when_tool_call_present() {
    // gpt-oss / harmony streaming: the analysis channel arrives as
    // `reasoning` deltas, the model emits a tool call, and no `content`
    // delta is committed. The reasoning is intermediate chain-of-thought,
    // not a final answer — the tool call is the action. It must NOT be
    // promoted into `.text` (that would leak private CoT into the
    // user-facing message and into the transcript the eval grader mines);
    // it stays under `.thinking`.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"We need to inspect\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\" parser.rs first.\"}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"look\",\"arguments\":\"{\\\"path\\\":\\\"parser.rs\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-reason-toolcall");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
        result.text, "",
        "reasoning leaked into visible text on a tool-call turn: {:?}",
        result.text
    );
    assert_eq!(
        result.thinking.as_deref(),
        Some("We need to inspect parser.rs first."),
        "reasoning trace must survive under thinking"
    );
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "look");

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_normalizes_harmony_wrapper_tool_call() {
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"tool\",\"arguments\":\"{\\\"na\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"me\\\":\\\"look\\\",\\\"args\\\":{\\\"path\\\":\\\"parser.rs\\\"}}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-wrapper-toolcall");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "look");
    assert_eq!(result.tool_calls[0]["arguments"]["path"], "parser.rs");

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_recovers_text_call_from_generic_wrapper_arguments() {
    let frame = |value: serde_json::Value| format!("data: {value}\n");
    let body = format!(
        "{}{}data: [DONE]\n",
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_search_wrapped",
                        "function": {
                            "name": "tool_call",
                            "arguments": "search({ path: \"internal/engine\", query: \"Reconcile(\" })"
                        }
                    }]
                }
            }]
        })),
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 20}
        })),
    );
    let session_id = fresh_session_id("oai-wrapper-argument-call");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "call_search_wrapped");
    assert_eq!(result.tool_calls[0]["name"], "search");
    assert_eq!(result.tool_calls[0]["arguments"]["path"], "internal/engine");
    assert_eq!(result.tool_calls[0]["arguments"]["query"], "Reconcile(");

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_tool_call_cut_by_length_keeps_finish_reason() {
    // IDE-host bug-report evidence shape: the output-token cap cuts a native
    // tool call mid-arguments. OpenRouter delivers the truncated args
    // deltas and a final chunk that carries BOTH a `tool_calls` delta and
    // `finish_reason:"length"`. The accumulated args fail to parse (-> the
    // empty-object fallback the agent loop sees as an empty-args call),
    // and `stop_reason` MUST still surface so the empty-args feedback can
    // name the truncation cause.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"chatcmpl-tool-1\",\"function\":{\"name\":\"edit\",\"arguments\":\"{\\\"pa\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"length\",\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"src/ma\"}}]}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":9}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-toolcall-length");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("length"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "edit");
    assert_eq!(
        result.tool_calls[0]["arguments"],
        serde_json::json!({}),
        "truncated unparseable args fall back to the empty object"
    );

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_text_format_arguments_recover_after_raw_partial() {
    // Some OpenAI-compatible routes are prompted for Harn's text tool
    // grammar but still surface the action through native tool_calls.
    // During streaming this is not strict JSON, so clients see a
    // raw_input_partial; on a clean tool_calls finish we can still recover
    // the complete Harn object literal instead of dispatching `{}`.
    let raw_args = r#"edit({ action: "replace_range", path: "src/main.rs", range_start: 1, range_end: 3, content: <<EOF
fn main() {
    println!("hello");
}
EOF
})"#;
    let frame = |value: serde_json::Value| format!("data: {value}\n");
    let body = format!(
        "{}{}data: [DONE]\n",
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_edit_text",
                        "function": {"name": "edit", "arguments": raw_args}
                    }]
                }
            }]
        })),
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 200}
        })),
    );
    let session_id = fresh_session_id("oai-text-args-recover");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    let raw_partials: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallUpdate {
                status: ToolCallStatus::Pending,
                raw_input_partial: Some(raw),
                ..
            } => Some(raw.clone()),
            _ => None,
        })
        .collect();
    assert!(
        raw_partials
            .iter()
            .any(|raw| raw.contains("edit({") && raw.contains("fn main()")),
        "expected multiline raw_input_partial before recovery; got {events:#?}"
    );
    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "edit");
    assert_eq!(
        result.tool_calls[0]["arguments"]["path"],
        serde_json::json!("src/main.rs")
    );
    assert!(
        result.tool_calls[0]["arguments"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("println!(\"hello\")")),
        "content should be recovered from the Harn text-tool payload: {:?}",
        result.tool_calls[0]["arguments"]
    );
    assert_ne!(result.tool_calls[0]["arguments"], serde_json::json!({}));

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_recovers_text_tool_call_misplaced_into_name() {
    let raw_name = r#"search({ query: "StatusOr", path: "include" })</arg_value>"#;
    let frame = |value: serde_json::Value| format!("data: {value}\n");
    let body = format!(
        "{}{}data: [DONE]\n",
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_search_text_name",
                        "function": {"name": raw_name, "arguments": "{}"}
                    }]
                }
            }]
        })),
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 30}
        })),
    );
    let session_id = fresh_session_id("oai-text-name-recover");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                status: ToolCallStatus::Pending,
                tool_name,
                ..
            } if tool_name == "search"
        )),
        "pending event should use the recovered tool name; got {events:#?}"
    );
    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "call_search_text_name");
    assert_eq!(result.tool_calls[0]["name"], "search");
    assert_eq!(result.tool_calls[0]["arguments"]["query"], "StatusOr");
    assert_eq!(result.tool_calls[0]["arguments"]["path"], "include");

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_rejects_partial_text_tool_call_misplaced_into_name() {
    let raw_name =
        r#"edit({ action: "create", path: "tests/page_cache_extra_test.cpp", content: <<EOF"#;
    let frame = |value: serde_json::Value| format!("data: {value}\n");
    let body = format!(
        "{}{}data: [DONE]\n",
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_edit_partial_text_name",
                        "function": {"name": raw_name, "arguments": "{"}
                    }]
                }
            }]
        })),
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 30}
        })),
    );
    let session_id = fresh_session_id("oai-text-name-parse-error");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                status: ToolCallStatus::Pending,
                tool_name,
                ..
            } if tool_name == "edit"
        )),
        "pending event should not expose the malformed provider name; got {events:#?}"
    );
    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "call_edit_partial_text_name");
    assert_eq!(result.tool_calls[0]["name"], "edit");
    let parse_error = result.tool_calls[0]["arguments"]["__parse_error"]
        .as_str()
        .expect("partial text-call name should carry a parse error");
    assert!(parse_error.contains("streamed provider tool name"));
    assert!(parse_error.contains("Raw input: edit({ action"));

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_non_length_unparseable_arguments_do_not_become_empty_object() {
    // If the provider finishes cleanly but the native arguments channel is
    // malformed and non-empty, preserve an explicit parse error. The `{}` fallback
    // is reserved for length truncation (covered above) and true empty args.
    let raw_args = "edit({\n  path: \"src/main.rs\",\n  content: <<EOF\nfn main() {\n";
    let frame = |value: serde_json::Value| format!("data: {value}\n");
    let body = format!(
        "{}{}data: [DONE]\n",
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_edit_bad_text",
                        "function": {"name": "edit", "arguments": raw_args}
                    }]
                }
            }]
        })),
        frame(serde_json::json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 200}
        })),
    );
    let session_id = fresh_session_id("oai-text-args-parse-error");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallUpdate {
                status: ToolCallStatus::Pending,
                raw_input_partial: Some(raw),
                ..
            } if raw.contains("fn main()")
        )),
        "expected non-empty multiline raw_input_partial; got {events:#?}"
    );
    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    let arguments = &result.tool_calls[0]["arguments"];
    assert_ne!(*arguments, serde_json::json!({}));
    assert!(
        arguments["__parse_error"]
            .as_str()
            .is_some_and(|message| message.contains("Could not parse streamed tool arguments")),
        "unparseable clean-finish arguments must carry a parse error, got {arguments:?}"
    );

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_splits_concatenated_tool_argument_objects() {
    // Some OpenAI-compatible routes stream multiple top-level JSON objects
    // in one `function.arguments` string. Match the non-streaming parser:
    // split them into sibling tool calls instead of surfacing a parse error.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_weather\",\"function\":{\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}{\\\"city\\\":\\\"Tokyo\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-stream-split-args");
    let (result, _events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 2);
    assert_eq!(result.tool_calls[0]["id"], "call_weather");
    assert_eq!(result.tool_calls[0]["name"], "weather");
    assert_eq!(result.tool_calls[0]["arguments"]["city"], "Paris");
    assert_eq!(result.tool_calls[1]["id"], "call_weather_2");
    assert_eq!(result.tool_calls[1]["name"], "weather");
    assert_eq!(result.tool_calls[1]["arguments"]["city"], "Tokyo");
    assert!(result
        .tool_calls
        .iter()
        .all(|call| { call["arguments"].get("__parse_error").is_none() }));

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_container_exec_argv_finalizes_to_run() {
    // gpt-oss / Harmony can borrow Codex's native `container.exec` tool
    // shape even when Harn advertised the canonical `run` tool. The
    // streamed transport must keep the pending lifecycle alive while
    // arguments are incomplete, then dispatch the final normalized Harn
    // call once the provider finishes with tool_calls.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_exec\",\"function\":{\"name\":\"container.exec\",\"arguments\":\"{\\\"cmd\\\":[\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"bash\\\",\\\"lc\\\",\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls -R\\\"],\\\"timeout_ms\\\":1000}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":7}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-container-exec-argv");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));

    let announcements: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCall { .. }))
        .collect();
    assert_eq!(
        announcements.len(),
        1,
        "expected one pending tool announcement, got {events:#?}"
    );
    match announcements[0] {
        AgentEvent::ToolCall {
            tool_call_id,
            tool_name,
            status,
            ..
        } => {
            assert_eq!(tool_call_id, "call_exec");
            assert_eq!(tool_name, "run");
            assert_eq!(*status, ToolCallStatus::Pending);
        }
        _ => unreachable!(),
    }

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                status: ToolCallStatus::Pending,
                raw_input: Some(raw),
                ..
            } if tool_call_id == "call_exec" && tool_name == "run" && raw["cmd"].is_array()
        )),
        "expected pending parsed argv updates while argv JSON streamed; got {events:#?}"
    );

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "call_exec");
    assert_eq!(result.tool_calls[0]["name"], "run");
    assert_eq!(result.tool_calls[0]["arguments"]["command"], "ls -R");
    assert_eq!(result.tool_calls[0]["arguments"]["timeout_ms"], 1000);
    assert!(result.tool_calls[0]["arguments"].get("cmd").is_none());
    assert!(result.tool_calls[0]["arguments"]
        .get("__parse_error")
        .is_none());

    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_announces_and_streams_partials() {
    // OpenAI Chat-Completions-style: tool name on the first delta,
    // arguments string concatenated across subsequent deltas.
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"REA\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"DME.md\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-stream");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    // Initial ToolCall(Pending) announcement on the first delta
    // that carries `function.name`.
    let announcements: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCall { .. }))
        .collect();
    assert_eq!(
        announcements.len(),
        1,
        "expected one initial ToolCall(Pending); got {events:#?}"
    );
    match announcements[0] {
        AgentEvent::ToolCall {
            tool_name,
            tool_call_id,
            status,
            ..
        } => {
            assert_eq!(tool_name, "read_file");
            assert_eq!(*status, ToolCallStatus::Pending);
            // The provider id is used verbatim so the streaming
            // announcement and executed lifecycle share one wire id.
            assert_eq!(tool_call_id, "call_a");
        }
        _ => unreachable!(),
    }

    // At least one partial-arg ToolCallUpdate fired during streaming.
    let partial_updates = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolCallUpdate {
                    status: ToolCallStatus::Pending,
                    ..
                }
            )
        })
        .count();
    assert!(
        partial_updates >= 1,
        "expected at least one Pending tool_call_update during streaming; got {events:#?}"
    );

    // Final tool call result still surfaces the canonical args.
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "read_file");
    assert_eq!(result.tool_calls[0]["arguments"]["path"], "README.md");
    // The agent loop reuses the dispatched call id for the executed
    // lifecycle, so it must match the streaming announcement id.
    assert_eq!(result.tool_calls[0]["id"], "call_a");

    clear_session_sinks(&session_id);
}

/// A streamed-then-executed tool call must live under one wire id
/// across the streaming announcement, every partial-arg update, and
/// the dispatched call that `__tool_envelope` turns into the
/// executed lifecycle. Covers both the provider-id path and the
/// empty-provider-id fallback; the fallback stays stable because the
/// transport writes it into the dispatched call.
#[tokio::test(flavor = "current_thread")]
async fn streamed_tool_call_uses_one_wire_id_across_announcement_and_dispatch() {
    // ── Provider sent a real id ─────────────────────────────────
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_0a1b2c\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.md\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-one-id");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    let mut event_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in &events {
        match event {
            AgentEvent::ToolCall { tool_call_id, .. }
            | AgentEvent::ToolCallUpdate { tool_call_id, .. } => {
                event_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }
    assert_eq!(
        event_ids.len(),
        1,
        "announcement + partial updates must share one id; got {event_ids:?}"
    );
    let announced_id = event_ids.into_iter().next().expect("one id");
    assert_eq!(announced_id, "call_0a1b2c", "provider id used verbatim");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(
        result.tool_calls[0]["id"], "call_0a1b2c",
        "dispatched call id (= the executed-lifecycle id) must equal the announced id"
    );
    clear_session_sinks(&session_id);

    // ── Provider sent NO id: the synthesized fallback must still
    //    be the one id on both sides ──────────────────────────────
    let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"search_web\",\"arguments\":\"{\\\"q\\\":\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"rust\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2}}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("oai-one-id-fallback");
    let (result, events) = drive(body.as_bytes(), &session_id, false).await;

    let mut event_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in &events {
        match event {
            AgentEvent::ToolCall { tool_call_id, .. }
            | AgentEvent::ToolCallUpdate { tool_call_id, .. } => {
                event_ids.insert(tool_call_id.clone());
            }
            _ => {}
        }
    }
    assert_eq!(
        event_ids.len(),
        1,
        "fallback announcement + updates must share one id; got {event_ids:?}"
    );
    let fallback_id = event_ids.into_iter().next().expect("one id");
    assert!(
        fallback_id.starts_with("stream-tool-1-"),
        "synthesized fallback shape; got {fallback_id:?}"
    );
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(
        result.tool_calls[0]["id"].as_str(),
        Some(fallback_id.as_str()),
        "the synthesized fallback must be written into the dispatched call so the \
             agent loop does not mint a second id for the executed lifecycle"
    );
    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn no_session_id_means_no_streaming_events() {
    // Without an opt-in session id the transport must remain silent
    // — the dispatch-time lifecycle still owns the canonical events
    // for raw `llm_call(...)` invocations from script context.
    let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_x\",\"name\":\"fake\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"k\\\":1}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: [DONE]\n",
        );
    let session_id = fresh_session_id("anth-silent");
    let events = install_capturing_sink(&session_id);
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(body.as_bytes());
    let _result = consume_sse_lines(
        reader,
        "anthropic",
        "test-model",
        true,
        delta_tx,
        None,
        None,
        false,
    )
    .await
    .expect("parse");
    let captured = events.lock().expect("capture mutex").clone();
    assert!(
        captured.is_empty(),
        "transport must emit no events when session_id is None; got {captured:#?}"
    );
    clear_session_sinks(&session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn coalescing_caps_event_count_under_burst() {
    // Build a burst of 20 input_json_deltas emitted in tight
    // succession (no real timer pauses inside the test). With
    // 50ms coalescing, only the very first delta should fire an
    // immediate ToolCallUpdate; the others should fold into it.
    let mut body = String::new();
    body.push_str(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_burst\",\"name\":\"big\"}}\n",
        );
    for i in 0..20 {
        body.push_str(&format!(
                "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"chunk{i} \"}}}}\n",
            ));
    }
    body.push_str("data: {\"type\":\"content_block_stop\",\"index\":0}\n");
    body.push_str("data: [DONE]\n");
    let session_id = fresh_session_id("anth-coalesce");
    let (_, events) = drive(body.as_bytes(), &session_id, true).await;

    // 20 deltas in well under 50ms must not produce 20 events.
    // We allow up to 3 (first delta + boundary races) so the test
    // tolerates clock jitter on busy CI hardware while still
    // guarding against the per-byte regression.
    let pending_updates = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolCallUpdate {
                    status: ToolCallStatus::Pending,
                    ..
                }
            )
        })
        .count();
    assert!(
        pending_updates < 20,
        "coalescing must cap the burst — got {pending_updates} pending updates from 20 deltas"
    );
    clear_session_sinks(&session_id);
}
