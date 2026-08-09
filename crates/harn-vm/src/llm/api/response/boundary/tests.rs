//! Response-ingestion boundary is loud (harn#5142, `response_content_extraction`).
//!
//! Ingestion is an allowlist of block/item/part types whose fall-through arms
//! used to discard whatever they did not name. The only prior signal was
//! `empty_generation_error`, which fires only when *every* block was dropped —
//! so one recognized text block alongside ten dropped ones passed as a complete
//! answer, and the provider quietly subtracted from every turn.

use serde_json::json;

use crate::agent_events::AgentEvent;
use crate::boundary::tests::CapturedEvents;
use crate::boundary::{BoundaryFailureKind, BoundaryId};

use crate::llm::api::response::{parse_llm_response, parse_openai_responses_response};
use crate::llm::capabilities::WireDialect;

fn details(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            AgentEvent::BoundaryFailure {
                boundary,
                kind,
                owner,
                detail,
                ..
            } => {
                assert_eq!(*boundary, BoundaryId::ResponseContentExtraction);
                assert_eq!(*kind, BoundaryFailureKind::Unrecognized);
                assert_eq!(owner, "harness");
                detail.clone()
            }
            other => panic!("expected a BoundaryFailure, got {other:?}"),
        })
        .collect()
}

#[test]
fn an_anthropic_content_block_with_no_handler_is_reported() {
    let captured = CapturedEvents::install();
    let response = json!({
        "content": [
            {"type": "text", "text": "here is the answer"},
            {"type": "some_future_block", "data": "opaque-blob"},
        ],
        "usage": {"output_tokens": 12},
    });
    parse_llm_response(
        &response,
        "anthropic",
        "claude-x",
        WireDialect::Anthropic,
        false,
    )
    .expect("a response with one usable block still parses");

    let reported = details(&captured.boundary_failures());
    assert_eq!(reported.len(), 1, "got: {reported:?}");
    assert!(
        reported[0].contains("some_future_block"),
        "the event must name the block type: {reported:?}",
    );
}

#[test]
fn a_fully_handled_anthropic_response_stays_quiet() {
    let captured = CapturedEvents::install();
    let response = json!({
        "content": [{"type": "text", "text": "all good"}],
        "usage": {"output_tokens": 3},
    });
    parse_llm_response(
        &response,
        "anthropic",
        "claude-x",
        WireDialect::Anthropic,
        false,
    )
    .expect("parses");
    assert!(
        captured.boundary_failures().is_empty(),
        "a fully handled response must not emit boundary noise",
    );
}

#[test]
fn an_openai_responses_output_item_with_no_handler_is_reported() {
    let captured = CapturedEvents::install();
    let response = json!({
        "output": [
            {"type": "message", "content": [{"type": "output_text", "text": "hello"}]},
            {"type": "some_future_item", "payload": {"a": 1}},
        ],
    });
    parse_openai_responses_response(&response, "openai", "gpt-x").expect("parses");

    let reported = details(&captured.boundary_failures());
    assert_eq!(reported.len(), 1, "got: {reported:?}");
    assert!(reported[0].contains("some_future_item"), "{reported:?}");
}

#[test]
fn an_openai_responses_message_part_with_no_string_text_is_reported() {
    let captured = CapturedEvents::install();
    let response = json!({
        "output": [{
            "type": "message",
            "content": [
                {"type": "output_text", "text": "hello"},
                {"type": "refusal", "refusal": {"reason": "policy"}},
            ],
        }],
    });
    parse_openai_responses_response(&response, "openai", "gpt-x").expect("parses");

    let reported = details(&captured.boundary_failures());
    assert_eq!(reported.len(), 1, "got: {reported:?}");
    assert!(reported[0].contains("refusal"), "{reported:?}");
}

/// `n > 1` is not a shape the runtime supports. Silently keeping one of several
/// completions made an unsupported request look like a served one.
#[test]
fn discarded_completions_past_the_first_choice_are_reported() {
    let captured = CapturedEvents::install();
    let response = json!({
        "choices": [
            {"message": {"role": "assistant", "content": "first"}, "finish_reason": "stop"},
            {"message": {"role": "assistant", "content": "second"}, "finish_reason": "stop"},
        ],
    });
    parse_llm_response(
        &response,
        "openai",
        "gpt-x",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parses");

    let reported = details(&captured.boundary_failures());
    assert_eq!(reported.len(), 1, "got: {reported:?}");
    assert!(reported[0].contains("2 choices"), "{reported:?}");
}
