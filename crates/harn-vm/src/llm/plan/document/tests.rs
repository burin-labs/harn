use super::*;

fn author(id: &str) -> PlanAuthor {
    PlanAuthor {
        id: id.to_string(),
        display_name: None,
    }
}

fn source(kind: &str) -> PlanSource {
    PlanSource {
        kind: kind.to_string(),
        uri: None,
    }
}

fn plan(content: &str) -> PlanArtifact {
    PlanArtifact {
        type_name: "plan_artifact".to_string(),
        schema_version: PLAN_SCHEMA_VERSION.to_string(),
        id: "plan_test".to_string(),
        tool: "update_plan".to_string(),
        title: "Test plan".to_string(),
        summary: "Exercise the collaborative contract.".to_string(),
        steps: vec![PlanStep {
            id: "step-1".to_string(),
            content: content.to_string(),
            status: PlanStepStatus::Pending,
            priority: None,
        }],
        assumptions: Vec::new(),
        open_questions: Vec::new(),
        verification_commands: vec!["make test".to_string()],
        approval: PlanApproval {
            state: PlanApprovalState::Unrequested,
            request_id: None,
            reviewer: None,
            reviewers: Vec::new(),
            approved_at: None,
            reason: None,
        },
    }
}

fn create_store() -> PlanDocumentStore {
    PlanDocumentStore::create(CreatePlanDocument {
        document_id: "plan-doc-1".to_string(),
        markdown: "# Test plan\n\n- First step".to_string(),
        plan: plan("First step"),
        author: author("alice"),
        source: source("user"),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        event_id: "event-create".to_string(),
    })
    .expect("create")
}

#[test]
fn create_edit_comment_resolve_reopen_and_replay_preserve_state() {
    let mut store = create_store();
    let created_revision = store.current().current_revision.revision_id.clone();

    store
        .edit(EditPlanDocument {
            expected_revision_id: created_revision,
            markdown: "# Test plan\n\n- Edited first step".to_string(),
            plan: plan("Edited first step"),
            author: author("alice"),
            source: source("editor"),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-edit".to_string(),
        })
        .expect("edit");
    let edited_revision = store.current().current_revision.revision_id.clone();
    assert_eq!(
        store
            .current()
            .current_revision
            .parent_revision_id
            .as_deref(),
        Some(
            store.events()[0]
                .document()
                .current_revision
                .revision_id
                .as_str()
        )
    );

    store
        .add_comment(AddPlanComment {
            expected_revision_id: edited_revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("Edited first step".to_string()),
                range: Some(PlanTextRange { start: 15, end: 32 }),
            },
            body: "Please make the verification explicit.".to_string(),
            author: author("reviewer"),
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let commented_revision = store.current().current_revision.revision_id.clone();

    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: commented_revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Resolved,
            author: author("agent"),
            source: source("agent"),
            created_at: "2026-01-01T00:03:00Z".to_string(),
            event_id: "event-resolve".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: Some("Added the exact command.".to_string()),
        })
        .expect("resolve");
    let resolved_revision = store.current().current_revision.revision_id.clone();
    let receipt = &store.current().resolution_receipts[0];
    assert_eq!(receipt.comment_id, "comment-1");
    assert_eq!(receipt.output_revision_id, resolved_revision);
    assert_eq!(receipt.agent_run_id, "run-1");
    assert_eq!(receipt.event_id, "event-resolve");
    assert_eq!(store.current().unresolved_comments().count(), 0);

    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: resolved_revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Reopened,
            author: author("reviewer"),
            source: source("review"),
            created_at: "2026-01-01T00:04:00Z".to_string(),
            event_id: "event-reopen".to_string(),
            agent_run_id: None,
            explanation: None,
        })
        .expect("reopen");
    assert_eq!(store.current().unresolved_comments().count(), 1);

    let persisted = serde_json::to_vec(store.events()).expect("serialize event log");
    let events: Vec<PlanDocumentEvent> =
        serde_json::from_slice(&persisted).expect("deserialize event log");
    let replayed = PlanDocumentStore::replay(&events).expect("replay");
    assert_eq!(replayed.current(), store.current());
    assert_eq!(
        replayed
            .current()
            .unresolved_comments()
            .map(|comment| comment.comment_id.as_str())
            .collect::<Vec<_>>(),
        vec!["comment-1"]
    );

    let artifacts = store
        .events()
        .iter()
        .map(PlanDocumentEvent::to_artifact_record)
        .collect::<Result<Vec<_>, _>>()
        .expect("persist through artifact lifecycle");
    let artifact_replay = PlanDocumentStore::replay_artifacts(&artifacts).expect("artifact replay");
    assert_eq!(artifact_replay.current(), store.current());
}

#[test]
fn concurrent_edits_fail_instead_of_overwriting_current_revision() {
    let mut store = create_store();
    let observed_revision = store.current().current_revision.revision_id.clone();
    let edit = |content: &str, event_id: &str| EditPlanDocument {
        expected_revision_id: observed_revision.clone(),
        markdown: format!("# Test plan\n\n- {content}"),
        plan: plan(content),
        author: author("alice"),
        source: source("editor"),
        created_at: "2026-01-01T00:01:00Z".to_string(),
        event_id: event_id.to_string(),
    };
    store
        .edit(edit("First writer", "event-edit-1"))
        .expect("first edit");
    let winner = store.current().clone();

    let error = store
        .edit(edit("Stale writer", "event-edit-2"))
        .expect_err("stale edit must conflict");
    assert!(matches!(
        error,
        PlanDocumentError::Conflict {
            expected_revision_id,
            current_revision_id,
            ..
        } if expected_revision_id == observed_revision
            && current_revision_id == winner.current_revision.revision_id
    ));
    assert_eq!(store.current(), &winner, "conflict must not mutate state");
}

#[test]
fn quoted_text_fallback_keeps_anchor_valid_after_step_and_range_move() {
    let mut store = create_store();
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("First step".to_string()),
                range: Some(PlanTextRange { start: 15, end: 25 }),
            },
            body: "Keep this intent.".to_string(),
            author: author("reviewer"),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let revision = store.current().current_revision.revision_id.clone();
    let mut replacement = plan("Rewritten");
    replacement.steps[0].id = "step-rewritten".to_string();
    store
        .edit(EditPlanDocument {
            expected_revision_id: revision,
            markdown: "# Short".to_string(),
            plan: replacement,
            author: author("alice"),
            source: source("editor"),
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-edit".to_string(),
        })
        .expect("quote fallback should preserve the anchor");
    store.current().validate().expect("document remains valid");
}

#[test]
fn addressed_then_resolved_records_each_agent_transition() {
    let mut store = create_store();
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: None,
                range: None,
            },
            body: "Address this.".to_string(),
            author: author("reviewer"),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Addressed,
            author: author("agent"),
            source: source("agent"),
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-address".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: None,
        })
        .expect("address");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Resolved,
            author: author("agent"),
            source: source("agent"),
            created_at: "2026-01-01T00:03:00Z".to_string(),
            event_id: "event-resolve".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: None,
        })
        .expect("resolve");
    assert_eq!(store.current().resolution_receipts.len(), 2);
    PlanDocumentStore::replay(store.events()).expect("replay both transitions");
}

#[test]
fn schema_rejects_unanchored_comments_and_validates_canonical_document() {
    let store = create_store();
    store.current().validate().expect("canonical document");
    let schema = plan_document_json_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let value = serde_json::to_value(store.current()).expect("document json");
    assert!(validator.is_valid(&value));

    let mut invalid = store.current().clone();
    invalid.comments.push(PlanComment {
        comment_id: "comment-invalid".to_string(),
        anchor: PlanCommentAnchor {
            step_id: None,
            quoted_text: None,
            range: None,
        },
        body: "No anchor".to_string(),
        state: PlanCommentState::Open,
        author: author("reviewer"),
        created_at: "2026-01-01T00:02:00Z".to_string(),
        updated_at: "2026-01-01T00:02:00Z".to_string(),
    });
    assert!(invalid.validate().is_err());
    let invalid_value = serde_json::to_value(invalid).expect("invalid document json");
    assert!(!validator.is_valid(&invalid_value));
}

#[test]
fn replay_rejects_a_broken_revision_chain() {
    let mut store = create_store();
    let current = store.current().current_revision.revision_id.clone();
    store
        .edit(EditPlanDocument {
            expected_revision_id: current,
            markdown: "# Test plan\n\n- Edited".to_string(),
            plan: plan("Edited"),
            author: author("alice"),
            source: source("editor"),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-edit".to_string(),
        })
        .expect("edit");
    let mut events = store.events().to_vec();
    let PlanDocumentEvent::Updated {
        input_revision_id, ..
    } = &mut events[1]
    else {
        panic!("updated event");
    };
    *input_revision_id = "stale-revision".to_string();
    assert!(matches!(
        PlanDocumentStore::replay(&events),
        Err(PlanDocumentError::Replay { .. })
    ));
}

#[test]
fn validation_rejects_a_tampered_resolution_receipt() {
    let mut store = create_store();
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("First step".to_string()),
                range: None,
            },
            body: "Explain the verification.".to_string(),
            author: author("reviewer"),
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
            author: author("agent"),
            source: source("agent"),
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-resolve".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: Some("Verification added.".to_string()),
        })
        .expect("resolve");

    let mut tampered = store.current().clone();
    tampered.resolution_receipts[0].agent_run_id = "different-run".to_string();

    assert!(matches!(
        tampered.validate(),
        Err(PlanDocumentError::Invalid(message))
            if message.contains("receipt_id does not match")
    ));
}

#[test]
fn validation_rejects_state_mutated_without_a_new_revision() {
    let mut store = create_store();
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("First step".to_string()),
                range: None,
            },
            body: "Original body.".to_string(),
            author: author("reviewer"),
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let mut tampered = store.current().clone();
    tampered.comments[0].body = "Changed without a revision.".to_string();
    assert!(matches!(
        tampered.validate(),
        Err(PlanDocumentError::Invalid(message))
            if message.contains("revision_id does not match")
    ));
}
