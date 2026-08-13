use super::{dict_get, vm_to_json};
use crate::value::VmValue;

/// Recover the plan artifact from a dispatched emit_plan/update_plan result.
///
/// The local short-circuit handler (`handle_tool_locally`) returns the
/// pretty-printed plan JSON as a string, so the dispatch result's
/// `result` field is typically a string. We try parsing it; if that
/// fails, fall back to renormalizing from the tool arguments. Either
/// way we get the executable plan embedded in the transcript's collaborative
/// `metadata.plan_document_event`.
pub(super) fn plan_artifact_from_result(result: &VmValue) -> Option<serde_json::Value> {
    if let Some(VmValue::String(rendered)) = dict_get(result, "result") {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rendered) {
            if parsed.is_object() {
                return Some(parsed);
            }
        }
    }
    if let Some(value) = dict_get(result, "result") {
        let json = vm_to_json(value);
        if json.is_object() {
            return Some(json);
        }
    }
    let tool_name = dict_get(result, "tool_name")
        .or_else(|| dict_get(result, "name"))
        .map(|v| v.display())
        .unwrap_or_default();
    let arguments = dict_get(result, "arguments").map(vm_to_json)?;
    Some(crate::llm::plan::normalize_plan_tool_call(
        &tool_name, &arguments,
    ))
}

pub(super) fn next_plan_document_events(
    session_id: &str,
    tool_name: &str,
    result: &VmValue,
    plan_value: serde_json::Value,
    created_at: String,
    event_id: String,
) -> Result<Vec<crate::llm::plan::PlanDocumentEvent>, crate::llm::plan::PlanDocumentError> {
    let events = crate::llm::plan::persisted_plan_document_events(session_id)?;
    if tool_name != crate::llm::plan::UPDATE_PLAN_TOOL || events.is_empty() {
        return crate::llm::plan::create_plan_document_event(
            plan_value, session_id, tool_name, created_at, event_id,
        )
        .map(|event| vec![event]);
    }

    let mut store = crate::llm::plan::resume_plan_document_store(session_id)?;
    let arguments = dict_get(result, "arguments")
        .map(vm_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(document_id) = arguments
        .get("document_id")
        .and_then(serde_json::Value::as_str)
    {
        if document_id != store.current().document_id {
            return Err(crate::llm::plan::PlanDocumentError::Invalid(format!(
                "update_plan document_id {document_id} does not match current document {}",
                store.current().document_id
            )));
        }
    }
    let expected_revision_id = arguments
        .get("expected_revision_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&store.current().current_revision.revision_id)
        .to_string();
    let plan: crate::llm::plan::PlanArtifact =
        serde_json::from_value(plan_value).map_err(|error| {
            crate::llm::plan::PlanDocumentError::Invalid(format!(
                "invalid executable plan: {error}"
            ))
        })?;
    let markdown = arguments
        .get("markdown")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            crate::llm::plan::render_plan(
                &serde_json::to_value(&plan).unwrap_or_else(|_| serde_json::json!({})),
            )
        });
    store.edit(crate::llm::plan::EditPlanDocument {
        expected_revision_id,
        markdown,
        plan,
        author: crate::llm::plan::PlanAuthor {
            id: session_id.to_string(),
            display_name: None,
        },
        source: crate::llm::plan::PlanSource {
            kind: tool_name.to_string(),
            uri: None,
        },
        created_at: created_at.clone(),
        event_id: event_id.clone(),
    })?;

    let mut emitted = vec![store
        .events()
        .last()
        .expect("edit appends a plan document event")
        .clone()];
    if let Some(resolutions) = arguments
        .get("comment_resolutions")
        .and_then(serde_json::Value::as_array)
    {
        for (index, resolution) in resolutions.iter().enumerate() {
            let comment_id = resolution
                .get("comment_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    crate::llm::plan::PlanDocumentError::Invalid(
                        "comment_resolutions entries require comment_id".to_string(),
                    )
                })?;
            let state = resolution
                .get("state")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| {
                    crate::llm::plan::PlanDocumentError::Invalid(format!(
                        "invalid comment resolution state: {error}"
                    ))
                })?
                .unwrap_or(crate::llm::plan::PlanCommentState::Resolved);
            let resolution_event_id = format!("{event_id}-comment-{}", index + 1);
            let agent_run_id =
                super::active_run_id(session_id).unwrap_or_else(|| session_id.to_string());
            store.change_comment_state(crate::llm::plan::ChangePlanCommentState {
                expected_revision_id: store.current().current_revision.revision_id.clone(),
                comment_id: comment_id.to_string(),
                state,
                author: crate::llm::plan::PlanAuthor {
                    id: session_id.to_string(),
                    display_name: None,
                },
                source: crate::llm::plan::PlanSource {
                    kind: tool_name.to_string(),
                    uri: None,
                },
                created_at: created_at.clone(),
                event_id: resolution_event_id,
                agent_run_id: Some(agent_run_id),
                explanation: resolution
                    .get("explanation")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
            })?;
            emitted.push(
                store
                    .events()
                    .last()
                    .expect("comment transition appends a plan document event")
                    .clone(),
            );
        }
    }
    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_selects_the_latest_created_plan_document() {
        crate::reset_thread_local_state();
        let session_id = "latest-plan-session";
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        let mut latest_document_id = String::new();
        for (index, content) in ["First plan", "Replacement plan"].iter().enumerate() {
            let plan = crate::llm::plan::normalize_plan_tool_call(
                crate::llm::plan::EMIT_PLAN_TOOL,
                &serde_json::json!({"steps": [{"content": content, "status": "pending"}]}),
            );
            let event = crate::llm::plan::create_plan_document_event(
                plan,
                "agent",
                "emit_plan",
                (100 + index).to_string(),
                format!("event-created-{index}"),
            )
            .expect("create plan");
            latest_document_id = event.document().document_id.clone();
            crate::llm::plan::persist_plan_document_event(session_id, &event)
                .expect("persist plan");
        }

        let resumed =
            crate::llm::plan::resume_plan_document_store(session_id).expect("resume latest plan");
        assert_eq!(resumed.current().document_id, latest_document_id);
        assert_eq!(
            resumed.current().current_revision.plan.steps[0].content,
            "Replacement plan"
        );
    }

    #[test]
    fn update_plan_emits_agent_resolution_receipt_after_edit() {
        use crate::llm::plan::{
            AddPlanComment, PlanAuthor, PlanCommentAnchor, PlanCommentState, PlanDocumentStore,
        };

        crate::reset_thread_local_state();
        let session_id = "plan-resolution-session";
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        let initial_plan = crate::llm::plan::normalize_plan_tool_call(
            crate::llm::plan::UPDATE_PLAN_TOOL,
            &serde_json::json!({
                "plan": [{"id": "step-1", "content": "Prove replay", "status": "pending"}]
            }),
        );
        let created = crate::llm::plan::create_plan_document_event(
            initial_plan,
            "agent",
            "update_plan",
            "100",
            "event-created",
        )
        .expect("create plan");
        crate::llm::plan::persist_plan_document_event(session_id, &created).expect("persist plan");
        let mut store = PlanDocumentStore::replay(&[created]).expect("replay plan");
        let created_revision_id = store.current().current_revision.revision_id.clone();
        store
            .add_comment(AddPlanComment {
                expected_revision_id: created_revision_id,
                comment_id: "review-1".to_string(),
                anchor: PlanCommentAnchor {
                    step_id: Some("step-1".to_string()),
                    quoted_text: Some("Prove replay".to_string()),
                    range: None,
                },
                body: "Exercise session/load.".to_string(),
                author: PlanAuthor {
                    id: "reviewer".to_string(),
                    display_name: None,
                },
                created_at: "101".to_string(),
                event_id: "event-comment".to_string(),
            })
            .expect("add comment");
        let comment_event = store.events().last().expect("comment event").clone();
        crate::llm::plan::persist_plan_document_event(session_id, &comment_event)
            .expect("persist comment");
        let expected_revision_id = store.current().current_revision.revision_id.clone();
        let document_id = store.current().document_id.clone();
        let arguments = serde_json::json!({
            "document_id": document_id,
            "expected_revision_id": expected_revision_id,
            "plan": [{"id": "step-1", "content": "Prove replay", "status": "completed"}],
            "comment_resolutions": [{
                "comment_id": "review-1",
                "state": "resolved",
                "explanation": "session/load now asserts the latest document"
            }]
        });
        let result = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "tool_name": "update_plan",
            "arguments": arguments,
        }));
        let plan = crate::llm::plan::normalize_plan_tool_call(
            crate::llm::plan::UPDATE_PLAN_TOOL,
            &arguments,
        );

        let events = next_plan_document_events(
            session_id,
            crate::llm::plan::UPDATE_PLAN_TOOL,
            &result,
            plan,
            "102".to_string(),
            "event-update".to_string(),
        )
        .expect("update and resolve comment");

        assert_eq!(events.len(), 2);
        let document = events.last().expect("resolution event").document();
        assert_eq!(document.comments[0].state, PlanCommentState::Resolved);
        assert_eq!(document.resolution_receipts.len(), 1);
        assert_eq!(document.resolution_receipts[0].agent_run_id, session_id);
        assert_eq!(
            document.resolution_receipts[0].output_revision_id,
            document.current_revision.revision_id
        );
    }
}
