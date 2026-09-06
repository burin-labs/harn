//! Session-event fixtures shared by the projection test modules.
//!
//! The payloads are transcribed from session
//! `019fc7e6-3103-7610-81ed-91599858fa1a` (issue #6120) rather than invented, so a
//! change to the emitter's envelope breaks these tests instead of quietly
//! producing an empty projection.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore, SessionEventKind};
use serde_json::json;
use std::collections::BTreeMap;

use super::super::*;

pub(super) fn custom(kind: &str) -> SessionEventKind {
    SessionEventKind::Custom {
        custom_type: kind.to_string(),
    }
}

pub(super) fn transcript_event(kind: &str, metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "transcript_event": {
            "id": format!("event-{kind}"),
            "kind": kind,
            "role": "assistant",
            "text": "",
            "metadata": metadata,
        }
    })
}

pub(super) fn llm_call(input: i64, output: i64, cost: f64) -> AppendEvent {
    AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "cache_read_tokens": 0,
                "cache_write_tokens": 10948,
                "cost_usd": cost,
                "input_tokens": input,
                "model": "gpt-5.6-luna",
                "output_tokens": output,
                "provider": "openai",
            }),
        ),
    )
}

pub(super) fn tool_call(id: &str, name: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::ToolCall,
        transcript_event(
            "tool_call",
            json!({
                "raw_input": {"file": "test/unit/cart_test.rb", "intent": "read"},
                "status": "pending",
                "tool_call_id": id,
                "tool_name": name,
            }),
        ),
    )
}

pub(super) fn tool_update(id: &str, name: &str, status: &str, duration_ms: i64) -> AppendEvent {
    AppendEvent::new(
        custom("tool_call_update"),
        transcript_event(
            "tool_call_update",
            json!({
                "duration_ms": duration_ms,
                "status": status,
                "tool_call_id": id,
                "tool_name": name,
            }),
        ),
    )
}

pub(super) fn tool_result(id: &str, text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::ToolResult,
        json!({
            "transcript_event": {
                "id": format!("result-{id}"),
                "kind": "tool_result",
                "role": "tool",
                "text": text,
                "metadata": {"tool_call_id": id},
            }
        }),
    )
}

pub(super) fn iteration_start(iteration: i64) -> AppendEvent {
    AppendEvent::new(
        custom("loop_checkpoint"),
        transcript_event(
            "loop_checkpoint",
            json!({"iteration": iteration, "kind": "iteration_start"}),
        ),
    )
}

pub(super) fn run_started() -> AppendEvent {
    AppendEvent::new(
        custom("agent_run_started"),
        transcript_event("agent_run_started", json!({"lifecycle_state": "running"})),
    )
}

pub(super) fn sub_agent_start(child_session_id: &str, child_run_id: &str) -> AppendEvent {
    AppendEvent::new(
        custom("sub_agent_start"),
        transcript_event(
            "sub_agent_start",
            json!({
                "child_session_id": child_session_id,
                "child_run_id": child_run_id,
            }),
        ),
    )
}

pub(super) fn terminal(final_status: &str, stop_reason: &str) -> AppendEvent {
    let kind = crate::agent_events::classify_agent_terminal(
        final_status,
        stop_reason,
        matches!(final_status, "error" | "failed" | "provider_error"),
        None,
    );
    AppendEvent::new(
        custom("agent_run_terminal"),
        transcript_event(
            "agent_run_terminal",
            json!({
                "error": null,
                "final_status": final_status,
                "stop_reason": stop_reason,
                "terminal_class": null,
                "terminal": crate::agent_events::AgentTerminalOutcome::new(kind, stop_reason),
            }),
        ),
    )
}

pub(super) fn legacy_terminal(final_status: &str, stop_reason: &str) -> AppendEvent {
    AppendEvent::new(
        custom("agent_run_terminal"),
        transcript_event(
            "agent_run_terminal",
            json!({
                "error": null,
                "final_status": final_status,
                "stop_reason": stop_reason,
                "terminal_class": null,
            }),
        ),
    )
}

pub(super) fn user_message(text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::Message,
        json!({
            "transcript_event": {"kind": "message", "role": "user", "text": text},
            "raw_message": {"content": text, "role": "user"},
        }),
    )
    .with_actor("user")
}

pub(super) fn assistant_message(text: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::Message,
        json!({
            "transcript_event": {
                "id": "assistant-visible",
                "kind": "message",
                "role": "assistant",
                "visibility": "public",
                "text": text,
            },
            "raw_message": {"content": text, "role": "assistant"},
        }),
    )
    .with_actor("assistant")
}

/// A store holding one session shaped like the run in #6120: a rate-limited,
/// pace-cut agent loop with tool calls and no run record anywhere.
pub(super) async fn capstone_like_store() -> (MemorySessionStore, String) {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession {
            id: Some("019fc7e6-3103-7610-81ed-91599858fa1a".to_string()),
            attributes: BTreeMap::from([
                ("source".to_string(), json!("burin-headless")),
                ("source_version".to_string(), json!("0.2.0")),
                ("source_revision".to_string(), json!("burin-sha")),
                ("harn_version".to_string(), json!("v0.10.84")),
                ("harn_revision".to_string(), json!("harn-sha")),
            ]),
            ..CreateSession::default()
        })
        .await
        .expect("create session");
    let id = meta.id.clone();
    for event in [
        user_message("Migrate the three unit test files."),
        iteration_start(1),
        llm_call(10951, 112, 0.002872),
        assistant_message("I inspected the three requested files."),
        tool_call("call_A", "look"),
        tool_update("call_A", "look", "in_progress", 0),
        tool_result("call_A", "[result of look]\n1\tclass CartTest"),
        tool_update("call_A", "look", "completed", 5),
        iteration_start(2),
        llm_call(20000, 300, 0.004),
        tool_call("call_B", "edit"),
        tool_update("call_B", "edit", "failed", 12),
        terminal("done", "pace_cutoff"),
    ] {
        store.append(&id, event).await.expect("append");
    }
    (store, id)
}

pub(super) fn llm_call_with_attempts(cost: f64, total: i64, rate_limited: i64) -> AppendEvent {
    AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "cost_usd": cost,
                "input_tokens": 100,
                "model": "gpt-5.6-luna",
                "output_tokens": 10,
                "provider": "openai",
                "provider_attempts": {
                    "total": total,
                    "retries": total - 1,
                    "rate_limited": rate_limited,
                    "empty_completion": 0,
                    "other": total - 1 - rate_limited,
                },
            }),
        ),
    )
}
