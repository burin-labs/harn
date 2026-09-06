//! Collaborative-plan records projected from the owning document schema.

use super::records::{FieldKind, Integer, Target};
use serde_json::Value;

const RECORDS: &[(&str, &str)] = &[
    ("author", "Author"),
    ("source", "Source"),
    ("plan_step", "Step"),
    ("approval", "Approval"),
    ("plan_artifact", "Artifact"),
    ("revision", "Revision"),
    ("anchor/properties/range", "TextRange"),
    ("anchor", "CommentAnchor"),
    ("comment", "Comment"),
    ("resolution_receipt", "CommentResolutionReceipt"),
    ("", "Document"),
];

pub(super) fn round_trip_fixture() -> Result<Value, String> {
    use harn_vm::llm::plan::*;
    let author = PlanAuthor {
        id: "reviewer".into(),
        display_name: Some("Reviewer".into()),
    };
    let source = PlanSource {
        kind: "user".into(),
        uri: Some("fixture://plan".into()),
    };
    let plan = serde_json::from_value(serde_json::json!({
        "_type": "plan_artifact", "schema_version": PLAN_SCHEMA_VERSION,
        "id": "plan-1", "tool": "update_plan", "title": "Binding proof", "summary": "Preserve fields",
        "steps": [{"id": "step-1", "content": "Verify", "status": "pending", "priority": "high"}],
        "assumptions": [], "open_questions": [], "verification_commands": ["make check-bindings"],
        "approval": {"state": "unrequested", "reviewers": ["reviewer"]}
    })).map_err(|error| format!("plan fixture: {error}"))?;
    let mut store = PlanDocumentStore::create(CreatePlanDocument {
        document_id: "document-1".into(),
        markdown: "Verify".into(),
        plan,
        author: author.clone(),
        source: source.clone(),
        created_at: "2026-01-01T00:00:00Z".into(),
        event_id: "create-1".into(),
    })
    .map_err(|error| error.to_string())?;
    store
        .add_comment(AddPlanComment {
            expected_revision_id: store.current().current_revision.revision_id.clone(),
            comment_id: "comment-1".into(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".into()),
                quoted_text: Some("Verify".into()),
                range: Some(PlanTextRange { start: 0, end: 6 }),
            },
            body: "Check codecs".into(),
            author: author.clone(),
            created_at: "2026-01-01T00:01:00Z".into(),
            event_id: "comment-1".into(),
        })
        .map_err(|error| error.to_string())?;
    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: store.current().current_revision.revision_id.clone(),
            comment_id: "comment-1".into(),
            state: PlanCommentState::Resolved,
            author,
            source,
            created_at: "2026-01-01T00:02:00Z".into(),
            event_id: "resolve-1".into(),
            agent_run_id: Some("run-1".into()),
            explanation: Some("Codecs checked".into()),
        })
        .map_err(|error| error.to_string())?;
    store
        .current()
        .validate()
        .map_err(|error| error.to_string())?;
    serde_json::to_value(store.current()).map_err(|error| error.to_string())
}

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_vm::llm::plan::plan_document_json_schema();
    let names = RECORDS
        .iter()
        .map(|(key, suffix)| (*key, format!("HarnPlan{suffix}")))
        .collect::<Vec<_>>();
    let mut records = super::schema_records::SchemaRecords {
        schema: &schema,
        names: &names,
        label: "plan",
        require_all: false,
        metadata,
    }
    .load()
    .expect("the owning plan schema projects to host records");
    for record in &mut records {
        for field in &mut record.fields {
            if record.name == "HarnPlanStep" {
                match (field.wire_name.as_ref(), target) {
                    ("status", Target::Typescript) => {
                        field.kind = FieldKind::Named("HarnPlanStepStatus".into())
                    }
                    ("priority", Target::Typescript | Target::Python) => {
                        field.kind = FieldKind::Json;
                        field.required = false;
                    }
                    _ => {}
                }
            }
        }
        if matches!(target, Target::Python) {
            // Python's positional constructor order is public; schema objects
            // have no member order. Preserve the two existing optional tails.
            let tail: &[&str] = match record.name.as_str() {
                "HarnPlanApproval" => &[
                    "request_id",
                    "reviewer",
                    "reviewers",
                    "approved_at",
                    "reason",
                ],
                "HarnPlanCommentAnchor" => &["step_id", "quoted_text", "range"],
                _ => &[],
            };
            record.fields.sort_by_key(|field| {
                (
                    !field.required,
                    tail.iter()
                        .position(|name| *name == field.wire_name)
                        .unwrap_or(0),
                )
            });
        }
        record.append_mutable(out, target);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
}

fn metadata(owner: &str, field: &str, schema: &Value) -> Result<Option<FieldKind>, String> {
    Ok(match (owner, field) {
        ("revision", "operation") if schema["type"] == "object" => {
            Some(FieldKind::Named("HarnPlanRevisionOperation".into()))
        }
        ("anchor", "range") if schema["type"] == "object" => {
            Some(FieldKind::Named("HarnPlanTextRange".into()))
        }
        ("approval", "state") if schema["enum"].is_array() => {
            Some(FieldKind::Named("HarnPlanApprovalState".into()))
        }
        ("comment", "state") if schema["enum"].is_array() => {
            Some(FieldKind::Named("HarnPlanCommentState".into()))
        }
        ("plan_step", "status") if schema["enum"].is_array() => Some(FieldKind::String),
        ("plan_step", "priority")
            if schema["type"] == serde_json::json!(["string", "integer", "null"]) =>
        {
            Some(FieldKind::Nullable(Box::new(FieldKind::Json)))
        }
        ("approval", "reviewers")
            if schema["type"] == "array" && schema["items"]["type"] == "string" =>
        {
            Some(FieldKind::DefaultList(Box::new(FieldKind::String)))
        }
        ("anchor/properties/range", _) if schema["type"] == "integer" => {
            Some(FieldKind::Integer(Integer::HostIndex))
        }
        _ => None,
    })
}
