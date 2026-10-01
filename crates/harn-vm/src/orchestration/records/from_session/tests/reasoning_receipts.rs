use super::super::*;
use super::support::*;
use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore, SessionStore};
use serde_json::json;

fn started(reported: bool) -> AppendEvent {
    AppendEvent::new(
        custom("agent_run_started"),
        transcript_event(
            "agent_run_started",
            json!({
                "reasoning_receipts_reported": reported,
            }),
        ),
    )
}

fn receipt() -> AppendEvent {
    AppendEvent::new(
        custom("reasoning_receipt"),
        transcript_event(
            "reasoning_receipt",
            json!({
                "receipt": crate::llm::ReasoningReceipt {
                    index: 99,
                    provider: "gemini".into(),
                    model: "gemini-3.6-flash".into(),
                    wire_dialect: "gemini_interactions".into(),
                    resolved_mode: "effort".into(),
                    resolved_level: Some("xhigh".into()),
                    sent_status: "carried".into(),
                    sent_field: Some("generation_config.thinking_level".into()),
                    sent_value: Some(json!("high")),
                    ..Default::default()
                }
            }),
        ),
    )
}

async fn project(events: Vec<AppendEvent>) -> RunRecord {
    let store = MemorySessionStore::default();
    let meta = store.create(CreateSession::default()).await.unwrap();
    for event in events {
        store.append(&meta.id, event).await.unwrap();
    }
    project_run_record_from_session(&store, &meta.id)
        .await
        .unwrap()
}

#[tokio::test]
async fn lowering_survives_projection_and_receipts_use_run_local_indices() {
    let run = project(vec![started(true), receipt(), receipt()]).await;
    let receipts = run.evidence.reasoning_receipts.unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].index, 0);
    assert_eq!(receipts[1].index, 1);
    assert_eq!(receipts[0].resolved_level.as_deref(), Some("xhigh"));
    assert_eq!(receipts[0].sent_value, Some(json!("high")));
}

#[tokio::test]
async fn zero_calls_are_measured_only_for_reporting_journals() {
    assert_eq!(
        project(vec![started(true)])
            .await
            .evidence
            .reasoning_receipts,
        Some(vec![])
    );
    assert_eq!(
        project(vec![
            AppendEvent::new(
                custom("agent_run_started"),
                transcript_event("agent_run_started", json!({}))
            ),
            llm_call(1, 1, 0.0),
        ])
        .await
        .evidence
        .reasoning_receipts,
        None
    );
    assert_eq!(project(vec![]).await.evidence.reasoning_receipts, None);
    assert_eq!(
        project(vec![started(true), receipt(), started(true)])
            .await
            .evidence
            .reasoning_receipts,
        Some(vec![])
    );
    assert_eq!(
        project(vec![started(true), receipt(), started(false)])
            .await
            .evidence
            .reasoning_receipts,
        None
    );
}

#[tokio::test]
async fn overflow_retains_the_prefix_and_reports_the_canonical_gap() {
    let mut events = vec![started(true)];
    events
        .extend((0..crate::llm::reasoning_receipt::MAX_REASONING_RECEIPTS + 3).map(|_| receipt()));
    let run = project(events).await;
    assert_eq!(
        run.evidence.reasoning_receipts.unwrap().len(),
        crate::llm::reasoning_receipt::MAX_REASONING_RECEIPTS
    );
    let gaps: Vec<_> = run
        .evidence
        .gaps
        .into_iter()
        .filter(|gap| gap.component == "reasoning_receipts")
        .collect();
    assert_eq!(
        gaps,
        vec![crate::llm::reasoning_receipt::overflow_gap(3).unwrap()]
    );
}
