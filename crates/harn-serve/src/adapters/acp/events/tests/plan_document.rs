use super::*;

pub(super) fn fixture_plan_document_event(
    plan: serde_json::Value,
) -> harn_vm::llm::plan::PlanDocumentEvent {
    let normalized = if plan
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        == Some(harn_vm::llm::plan::PLAN_SCHEMA_VERSION)
    {
        plan
    } else {
        harn_vm::llm::plan::normalize_plan_tool_call(
            harn_vm::llm::plan::UPDATE_PLAN_TOOL,
            &serde_json::json!({"plan": plan}),
        )
    };
    harn_vm::llm::plan::create_plan_document_event(
        normalized,
        "test-agent",
        "test",
        "2026-01-01T00:00:00Z",
        "plan-event-test",
    )
    .expect("fixture plan document")
}

#[tokio::test(flavor = "current_thread")]
async fn structured_plan_extension_fixture_is_pinned() {
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::EMIT_PLAN_TOOL,
        &serde_json::json!({
            "summary": "Ship plan events.",
            "steps": [
                {"content": "Emit plan event.", "status": "completed"},
                {"content": "Verify fixtures.", "status": "pending"}
            ],
            "verification_commands": ["cargo test -p harn-serve acp"],
        }),
    );
    let actual = collect_notifications(vec![AgentEvent::PlanDocumentUpdated {
        session_id: "session-1".to_string(),
        event: Box::new(fixture_plan_document_event(plan)),
    }])
    .await;
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/acp/session_update_plan_extension.json"
    ))
    .expect("fixture json");
    assert_eq!(serde_json::Value::Array(actual), expected);
}

#[tokio::test(flavor = "current_thread")]
async fn collaborative_plan_receipt_round_trips_over_acp() {
    use harn_vm::llm::plan::{
        AddPlanComment, ChangePlanCommentState, PlanAuthor, PlanCommentAnchor, PlanCommentState,
        PlanDocumentStore, PlanSource,
    };

    let created = fixture_plan_document_event(harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({"plan": [{"step": "Ship it.", "status": "pending"}]}),
    ));
    let mut store = PlanDocumentStore::replay(&[created]).expect("created document");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("Ship it.".to_string()),
                range: None,
            },
            body: "Prove the release gate.".to_string(),
            author: PlanAuthor {
                id: "reviewer".to_string(),
                display_name: None,
            },
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Resolved,
            author: PlanAuthor {
                id: "agent".to_string(),
                display_name: None,
            },
            source: PlanSource {
                kind: "agent".to_string(),
                uri: None,
            },
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-resolve".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: Some("Release gate passed.".to_string()),
        })
        .expect("resolve");

    let event = store.events().last().expect("updated event").clone();
    let expected_revision = store.current().current_revision.revision_id.clone();
    let actual = collect_notifications(vec![AgentEvent::PlanDocumentUpdated {
        session_id: "session-1".to_string(),
        event: Box::new(event),
    }])
    .await;
    let document = &actual[0]["params"]["update"]["harnPlanDocument"];
    assert_eq!(
        document["current_revision"]["revision_id"],
        expected_revision
    );
    assert_eq!(
        document["resolution_receipts"][0]["input_revision_id"],
        store.current().resolution_receipts[0].input_revision_id
    );
    assert_eq!(
        document["resolution_receipts"][0]["output_revision_id"],
        expected_revision
    );
    assert_eq!(
        document["resolution_receipts"][0]["event_id"],
        "event-resolve"
    );
}
