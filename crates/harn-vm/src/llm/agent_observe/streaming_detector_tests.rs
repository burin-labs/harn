//! Verify the streaming candidate detector glue (harn#692). The
//! unit tests in `crate::llm::tools::parse::streaming` already cover
//! the detector's state machine; these tests cover the loop body
//! that pumps deltas through the detector and dispatches each event
//! to a sink. Uses `run_detector_loop_with_sink` with a captured
//! buffer so the test doesn't depend on the global session sink
//! registry — other tests in this binary mutate the registry via
//! `reset_all_sinks` and can race a per-session install otherwise.
use std::cell::RefCell;
use std::rc::Rc;

use crate::agent_events::{AgentEvent, ToolCallStatus};

use super::{run_detector_loop_with_sink, StreamingDetectorContext};

/// Pipe `chunks` through `run_detector_loop_with_sink`, await its
/// completion, and return the captured events in arrival order.
async fn drive(session_id: &str, known: &[&str], chunks: &[&str]) -> Vec<AgentEvent> {
    let captured: Rc<RefCell<Vec<AgentEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let sink_buf = captured.clone();
    let known_tools = known.iter().map(|s| (*s).to_string()).collect();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    for chunk in chunks {
        tx.send((*chunk).to_string()).expect("send delta");
    }
    drop(tx);
    run_detector_loop_with_sink(
        StreamingDetectorContext {
            session_id: session_id.to_string(),
            known_tools,
        },
        rx,
        move |event| sink_buf.borrow_mut().push(event.clone()),
    )
    .await;
    let events = captured.borrow().clone();
    events
}

#[tokio::test(flavor = "current_thread")]
async fn detector_loop_emits_start_and_promoted_through_sink() {
    let events = drive(
        "session-stream-promote",
        &["read"],
        &["read({ path: \"a.md\" })"],
    )
    .await;
    assert_eq!(
        events.len(),
        2,
        "expected start + promoted, got: {events:#?}"
    );
    match &events[0] {
        AgentEvent::ToolCall {
            parsing,
            tool_name,
            status,
            ..
        } => {
            assert_eq!(*parsing, Some(true));
            assert_eq!(tool_name, "read");
            assert_eq!(*status, ToolCallStatus::Pending);
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
    match &events[1] {
        AgentEvent::ToolCallUpdate {
            parsing,
            status,
            error_category,
            raw_input,
            raw_output,
            ..
        } => {
            assert_eq!(*parsing, Some(false));
            assert_eq!(*status, ToolCallStatus::Pending);
            assert!(error_category.is_none());
            assert_eq!(
                raw_input.as_ref(),
                Some(&serde_json::json!({"path": "a.md"}))
            );
            assert!(raw_output.is_none(), "promoted args belong in raw_input");
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn detector_loop_finalizes_unclosed_tagged_block_as_aborted() {
    let events = drive(
        "session-stream-abort",
        &["run"],
        &["<tool_call>\nrun({ command: \"ls\""],
    )
    .await;
    assert_eq!(events.len(), 2, "events={events:#?}");
    match &events[1] {
        AgentEvent::ToolCallUpdate {
            status,
            error_category,
            parsing,
            ..
        } => {
            assert_eq!(*status, ToolCallStatus::Failed);
            assert_eq!(
                *error_category,
                Some(crate::agent_events::ToolCallErrorCategory::ParseAborted)
            );
            assert_eq!(*parsing, Some(false));
        }
        other => panic!("expected ToolCallUpdate, got {other:?}"),
    }
}
