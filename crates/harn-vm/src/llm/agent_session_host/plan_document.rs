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

fn persisted_plan_document_events(
    session_id: &str,
) -> Result<Vec<crate::llm::plan::PlanDocumentEvent>, crate::llm::plan::PlanDocumentError> {
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return Ok(Vec::new());
    };
    vm_to_json(&transcript)
        .get("events")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|event| event.pointer("/metadata/plan_document_event"))
        .map(|event| {
            serde_json::from_value(event.clone()).map_err(|error| {
                crate::llm::plan::PlanDocumentError::Invalid(format!(
                    "invalid persisted plan document event: {error}"
                ))
            })
        })
        .collect()
}

pub(super) fn next_plan_document_event(
    session_id: &str,
    tool_name: &str,
    result: &VmValue,
    plan_value: serde_json::Value,
    created_at: String,
    event_id: String,
) -> Result<crate::llm::plan::PlanDocumentEvent, crate::llm::plan::PlanDocumentError> {
    let events = persisted_plan_document_events(session_id)?;
    if tool_name != crate::llm::plan::UPDATE_PLAN_TOOL || events.is_empty() {
        return crate::llm::plan::create_plan_document_event(
            plan_value, session_id, tool_name, created_at, event_id,
        );
    }

    let mut store = if matches!(
        events.first(),
        Some(crate::llm::plan::PlanDocumentEvent::Created { .. })
    ) {
        crate::llm::plan::PlanDocumentStore::replay(&events)?
    } else {
        crate::llm::plan::PlanDocumentStore::resume(
            events
                .last()
                .expect("non-empty plan document event list")
                .document()
                .clone(),
        )?
    };
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
        created_at,
        event_id,
    })?;
    Ok(store
        .events()
        .last()
        .expect("edit appends a plan document event")
        .clone())
}
