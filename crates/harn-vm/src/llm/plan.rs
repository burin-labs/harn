//! Harn-owned plan artifact normalization and tool helpers.

use sha2::{Digest, Sha256};

mod document;

pub use document::*;

pub const PLAN_SCHEMA_VERSION: &str = "harn.plan.v1";
pub const EMIT_PLAN_TOOL: &str = "emit_plan";
pub const UPDATE_PLAN_TOOL: &str = "update_plan";

/// Timestamp used by canonical plan revisions and receipts.
pub fn plan_timestamp() -> String {
    crate::orchestration::now_unix_seconds_text()
}

/// Allocate an event id in the canonical plan-document namespace.
pub fn new_plan_event_id() -> String {
    crate::orchestration::new_id("plan_event")
}

/// Allocate a stable id for a newly-added plan comment.
pub fn new_plan_comment_id() -> String {
    crate::orchestration::new_id("plan_comment")
}

pub fn is_plan_tool(name: &str) -> bool {
    matches!(name, EMIT_PLAN_TOOL | UPDATE_PLAN_TOOL)
}

pub fn normalize_plan_tool_call(tool_name: &str, args: &serde_json::Value) -> serde_json::Value {
    let source = args.get("plan").unwrap_or(args);
    let mut plan = serde_json::json!({
        "_type": "plan_artifact",
        "schema_version": PLAN_SCHEMA_VERSION,
        "tool": tool_name,
        "title": string_field(source, &["title", "name"]).unwrap_or_else(|| "Plan".to_string()),
        "summary": string_field(args, &["explanation", "summary", "direction"])
            .or_else(|| string_field(source, &["explanation", "summary", "direction"]))
            .unwrap_or_default(),
        "steps": normalize_steps(source),
        "assumptions": string_list_field(source, &["assumptions"]),
        "open_questions": string_list_field(source, &["open_questions", "questions", "unknowns"]),
        "verification_commands": string_list_field(
            source,
            &["verification_commands", "verification", "verify_commands"],
        ),
        "approval": normalize_approval(source.get("approval").or_else(|| args.get("approval"))),
    });

    let digest_input = crate::canonical_json::to_vec(&plan);
    let digest = hex::encode(Sha256::digest(digest_input));
    #[expect(clippy::string_slice, reason = "hex digest is ASCII")]
    let id = format!("plan_{}", &digest[..12]);
    plan["id"] = serde_json::Value::String(id);
    plan
}

pub fn create_plan_document(
    plan: serde_json::Value,
    author_id: impl Into<String>,
    source_kind: impl Into<String>,
    created_at: impl Into<String>,
    event_id: impl Into<String>,
) -> Result<PlanDocument, PlanDocumentError> {
    Ok(
        create_plan_document_event(plan, author_id, source_kind, created_at, event_id)?
            .document()
            .clone(),
    )
}

pub fn create_plan_document_event(
    plan: serde_json::Value,
    author_id: impl Into<String>,
    source_kind: impl Into<String>,
    created_at: impl Into<String>,
    event_id: impl Into<String>,
) -> Result<PlanDocumentEvent, PlanDocumentError> {
    let plan: PlanArtifact = serde_json::from_value(plan)
        .map_err(|error| PlanDocumentError::Invalid(format!("invalid executable plan: {error}")))?;
    let document_id = format!("plan_document_{}", plan.id.trim_start_matches("plan_"));
    let markdown = render_plan(&serde_json::to_value(&plan).map_err(|error| {
        PlanDocumentError::Invalid(format!("cannot render executable plan: {error}"))
    })?);
    let store = PlanDocumentStore::create(CreatePlanDocument {
        document_id,
        markdown,
        plan,
        author: PlanAuthor {
            id: author_id.into(),
            display_name: None,
        },
        source: PlanSource {
            kind: source_kind.into(),
            uri: None,
        },
        created_at: created_at.into(),
        event_id: event_id.into(),
    })?;
    Ok(store.events()[0].clone())
}

pub fn plan_entries(plan: &serde_json::Value) -> serde_json::Value {
    let entries = plan
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| {
                    let content = step.get("content")?.as_str()?.trim();
                    if content.is_empty() {
                        return None;
                    }
                    let mut entry = serde_json::json!({
                        "content": content,
                        "status": step.get("status").and_then(serde_json::Value::as_str).unwrap_or("pending"),
                    });
                    if let Some(priority) = step.get("priority") {
                        if !priority.is_null() {
                            entry["priority"] = priority.clone();
                        }
                    }
                    Some(entry)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if entries.is_empty() {
        let summary = plan
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim();
        let content = if summary.is_empty() {
            "Plan emitted"
        } else {
            summary
        };
        return serde_json::json!([{ "content": content, "status": "pending" }]);
    }
    serde_json::Value::Array(entries)
}

pub fn plan_document_entries(document: &PlanDocument) -> serde_json::Value {
    serde_json::to_value(&document.current_revision.plan)
        .map(|plan| plan_entries(&plan))
        .unwrap_or_else(|_| serde_json::json!([]))
}

/// Recover every canonical plan-document event persisted in an agent session.
///
/// The transcript is the semantic source of truth. ACP/A2A replay and client
/// mutations both use this reader so a host never needs a parallel plan store.
pub fn persisted_plan_document_events(
    session_id: &str,
) -> Result<Vec<PlanDocumentEvent>, PlanDocumentError> {
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return Ok(Vec::new());
    };
    crate::llm::helpers::vm_value_to_json(&transcript)
        .get("events")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|event| event.pointer("/metadata/plan_document_event"))
        .map(|event| {
            serde_json::from_value(event.clone()).map_err(|error| {
                PlanDocumentError::Invalid(format!(
                    "invalid persisted plan document event: {error}"
                ))
            })
        })
        .collect()
}

/// Resume the canonical store for a session from transcript-owned events.
pub fn resume_plan_document_store(
    session_id: &str,
) -> Result<PlanDocumentStore, PlanDocumentError> {
    let events = persisted_plan_document_events(session_id)?;
    if events.is_empty() {
        return Err(PlanDocumentError::Invalid(format!(
            "session {session_id} has no collaborative plan document"
        )));
    }
    if let Some(latest_created) = events
        .iter()
        .rposition(|event| matches!(event, PlanDocumentEvent::Created { .. }))
    {
        PlanDocumentStore::replay(&events[latest_created..])
    } else {
        PlanDocumentStore::resume(
            events
                .last()
                .expect("non-empty plan document event list")
                .document()
                .clone(),
        )
    }
}

/// Append one canonical plan mutation to the owning session transcript.
///
/// Event fanout remains the caller's responsibility because an agent-tool
/// turn already has live sinks installed while an idle ACP client mutation
/// must install its transport sink before emitting.
pub fn persist_plan_document_event(
    session_id: &str,
    event: &PlanDocumentEvent,
) -> Result<(), PlanDocumentError> {
    let metadata = serde_json::json!({
        "plan_document": event.document(),
        "plan_document_event": event,
    });
    let transcript_event = crate::llm::helpers::transcript_event(
        "plan_document",
        "tool",
        "public",
        "",
        Some(metadata),
    );
    crate::agent_sessions::append_event(session_id, transcript_event)
        .map_err(PlanDocumentError::Invalid)
}

pub fn render_plan(plan: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    let title = plan
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Plan");
    lines.push(format!("# {title}"));
    if let Some(summary) = plan.get("summary").and_then(serde_json::Value::as_str) {
        if !summary.trim().is_empty() {
            lines.push(String::new());
            lines.push(summary.trim().to_string());
        }
    }
    if let Some(steps) = plan.get("steps").and_then(serde_json::Value::as_array) {
        if !steps.is_empty() {
            lines.push(String::new());
            lines.push("## Steps".to_string());
            for step in steps {
                let status = step
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("pending");
                let content = step
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                lines.push(format!("- [{status}] {content}"));
            }
        }
    }
    append_list_section(&mut lines, plan, "assumptions", "Assumptions");
    append_list_section(&mut lines, plan, "open_questions", "Open questions");
    append_list_section(
        &mut lines,
        plan,
        "verification_commands",
        "Verification commands",
    );
    let approval_state = plan
        .pointer("/approval/state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unrequested");
    lines.push(String::new());
    lines.push(format!("Approval: {approval_state}"));
    lines.join("\n")
}

fn append_list_section(lines: &mut Vec<String>, plan: &serde_json::Value, key: &str, title: &str) {
    let Some(items) = plan.get(key).and_then(serde_json::Value::as_array) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("## {title}"));
    for item in items {
        if let Some(text) = item.as_str() {
            lines.push(format!("- {text}"));
        }
    }
}

fn normalize_steps(source: &serde_json::Value) -> serde_json::Value {
    let steps_value = source
        .get("steps")
        .or_else(|| source.get("plan"))
        .or_else(|| source.get("tasks"))
        .unwrap_or(source);
    let Some(items) = steps_value.as_array() else {
        return serde_json::Value::Array(Vec::new());
    };
    let steps = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| normalize_step(idx, item))
        .collect::<Vec<_>>();
    serde_json::Value::Array(steps)
}

fn normalize_step(idx: usize, item: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(text) = item.as_str() {
        let content = text.trim();
        if content.is_empty() {
            return None;
        }
        return Some(serde_json::json!({
            "id": format!("step-{}", idx + 1),
            "content": content,
            "status": "pending",
            "priority": null,
        }));
    }
    let object = item.as_object()?;
    let content = string_field(item, &["content", "step", "task", "goal", "description"])?;
    if content.trim().is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "id": string_field(item, &["id"]).unwrap_or_else(|| format!("step-{}", idx + 1)),
        "content": content.trim(),
        "status": normalize_status(object.get("status")),
        "priority": object.get("priority").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

fn normalize_status(value: Option<&serde_json::Value>) -> &'static str {
    match value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("pending")
    {
        "pending" | "todo" | "not_started" => "pending",
        "in_progress" | "active" | "running" | "started" => "in_progress",
        "completed" | "complete" | "done" => "completed",
        "blocked" | "waiting" => "blocked",
        "cancelled" | "canceled" | "dropped" | "skipped" => "cancelled",
        _ => "pending",
    }
}

fn normalize_approval(value: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(value) = value else {
        return serde_json::json!({"state": "unrequested"});
    };
    if let Some(object) = value.as_object() {
        let mut approval = serde_json::json!({
            "state": approval_state(value),
        });
        for key in [
            "request_id",
            "reviewer",
            "reviewers",
            "approved_at",
            "reason",
        ] {
            if let Some(field) = object.get(key) {
                approval[key] = field.clone();
            }
        }
        return approval;
    }
    serde_json::json!({"state": approval_state(value)})
}

fn approval_state(value: &serde_json::Value) -> &'static str {
    if let Some(state) = value
        .get("state")
        .or_else(|| value.get("status"))
        .and_then(serde_json::Value::as_str)
    {
        return match state {
            "approved" => "approved",
            "rejected" | "denied" => "rejected",
            "requested" | "pending" => "requested",
            _ => "unrequested",
        };
    }
    match value.get("approved").and_then(serde_json::Value::as_bool) {
        Some(true) => "approved",
        Some(false) => "rejected",
        None => "unrequested",
    }
}

fn string_list_field(source: &serde_json::Value, keys: &[&str]) -> serde_json::Value {
    for key in keys {
        if let Some(value) = source.get(*key) {
            return serde_json::Value::Array(string_list(value));
        }
    }
    serde_json::Value::Array(Vec::new())
}

fn string_list(value: &serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| scalar_text(item).map(serde_json::Value::String))
            .filter(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
            .map(|item| {
                serde_json::Value::String(item.as_str().unwrap_or_default().trim().to_string())
            })
            .collect(),
        _ => scalar_text(value)
            .filter(|text| !text.trim().is_empty())
            .map(|text| serde_json::Value::String(text.trim().to_string()))
            .into_iter()
            .collect(),
    }
}

fn string_field(source: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| source.get(*key).and_then(scalar_text))
}

fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn update_plan_shape_normalizes_to_artifact_and_acp_entries() {
        let plan = normalize_plan_tool_call(
            UPDATE_PLAN_TOOL,
            &json!({
                "explanation": "Adjust after reading the parser.",
                "plan": [
                    {"step": "Read parser tests.", "status": "completed"},
                    {"step": "Patch parser.", "status": "in_progress"}
                ],
                "verification_commands": ["cargo test -p harn-parser"],
                "approval": {"approved": true, "reviewers": ["lead"]}
            }),
        );
        assert_eq!(plan["schema_version"], PLAN_SCHEMA_VERSION);
        assert_eq!(plan["steps"][0]["content"], "Read parser tests.");
        assert_eq!(plan["steps"][1]["status"], "in_progress");
        assert_eq!(plan["approval"]["state"], "approved");

        let entries = plan_entries(&plan);
        assert_eq!(entries[0]["content"], "Read parser tests.");
        assert_eq!(entries[0]["status"], "completed");
    }

    #[test]
    fn empty_plan_entries_use_non_empty_fallback() {
        let plan = normalize_plan_tool_call(EMIT_PLAN_TOOL, &json!({"summary": ""}));

        let entries = plan_entries(&plan);
        assert_eq!(entries[0]["content"], "Plan emitted");
        assert_eq!(entries[0]["status"], "pending");
    }
}
