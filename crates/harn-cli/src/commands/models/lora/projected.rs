//! Convert Harn-projected training examples for `harn models lora export`.
//!
//! A `harn.agent_training_example.v1` row (see `harn runs export-training`)
//! already carries typed calls, typed results, and the exact tool catalog the
//! model was served. So this path re-checks the pairing invariant with the
//! projector's own validator — one owner, so producer and consumer cannot
//! drift into different readings of it — and renders the canonical messages
//! into the requested dataset format.
//!
//! What it deliberately does not do: re-parse wire text, match tool names with
//! regexes against the system prompt, or synthesize a JSON Schema from the
//! argument values a run happened to use. The last one is the reason projected
//! rows exist: an inferred schema can teach a tool narrower than the one the
//! model actually saw.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::export::{
    export_metadata, normalized_message, record_id, record_string, ConvertedExport, ExportTarget,
};

/// Does this corpus row carry Harn's authoritative projection?
///
/// A projected row already holds typed calls, typed results, and the exact
/// served tool catalog, so it takes a separate conversion path that neither
/// re-parses wire text nor infers a schema.
pub(super) fn is_projected_example(record: &Map<String, Value>) -> bool {
    record.get("schema_version").and_then(Value::as_str)
        == Some(harn_vm::orchestration::TRAINING_EXAMPLE_SCHEMA_VERSION)
}

/// Convert one `harn.agent_training_example.v1` row.
///
/// Everything the trainer needs is already typed and already verified
/// upstream, so this path only re-checks the pairing invariant (using the same
/// validator the projector used, so the two can never drift) and renders the
/// canonical messages into the requested dataset format. There is no second
/// wire parser, no regex over prompt prose, and no schema synthesized from
/// observed argument values — a projected row's catalog flows straight
/// through, which is the whole point of projecting it.
pub(super) fn convert_projected_example(
    record: &Map<String, Value>,
    target: &ExportTarget,
    dataset_format: &str,
    behavior_classes: &BTreeSet<String>,
) -> Result<ConvertedExport, String> {
    let example: harn_vm::orchestration::AgentTrainingExample =
        serde_json::from_value(Value::Object(record.clone()))
            .map_err(|error| format!("row is not a valid projected training example: {error}"))?;
    harn_vm::orchestration::validate_training_example_pairing(&example.messages)
        .map_err(|error| error.to_string())?;
    if example.tools.is_empty() {
        return Err("projected example carries an empty tool catalog".to_string());
    }
    // Render the served catalog into the trainer's function-schema shape with
    // Harn's own converter. Every declared parameter survives, including the
    // ones this run never passed — the case where inferring a schema from
    // observed arguments would teach a narrower tool than the model saw.
    let tools = example
        .tools
        .iter()
        .map(|row| {
            harn_vm::orchestration::function_schema_from_catalog_row(row).ok_or_else(|| {
                format!("projected example carries a tool catalog row Harn cannot render: {row}")
            })
        })
        .collect::<Result<Vec<Value>, String>>()?;
    let declared_format = example.provenance.effective_tool_format.as_str();
    let system_text = example
        .messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let metadata = projected_metadata(
        record,
        target,
        dataset_format,
        &example,
        &tools,
        &system_text,
    );
    let tool_calls = example
        .messages
        .iter()
        .map(|message| message.tool_calls.len() as u64)
        .sum();
    let tool_results = example
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .count() as u64;

    if dataset_format == "messages_with_tool_calls" {
        let row = json!({
            "id": record_string(record, "id"),
            "eval_name": record_string(record, "eval_name"),
            "language": record_string(record, "language"),
            "task_type": record_string(record, "task_type"),
            "messages": example.messages,
            "tools": tools,
            "metadata": metadata,
        });
        return Ok(ConvertedExport {
            rows: vec![row],
            tool_calls,
            tool_results,
            behavior_classes: behavior_classes.clone(),
        });
    }

    // A text-channel dataset trains on the assistant's served text, which the
    // projection preserved byte-exact. Rendering a native-channel run into it
    // would have to invent the tagged block the model never emitted.
    if declared_format != "text" && declared_format != "json" {
        return Err(format!(
            "cannot export a projected example served with tool_format={declared_format} as \
             `{dataset_format}`; that dataset trains on text-channel assistant turns"
        ));
    }
    let mut rows = Vec::new();
    let mut context: Vec<Value> = Vec::new();
    for (index, message) in example.messages.iter().enumerate() {
        if message.role == "assistant" && !message.tool_calls.is_empty() {
            rows.push(json!({
                "id": format!("{}#turn-{index}", record_id(record, index + 1)),
                "source_id": record_string(record, "id"),
                "eval_name": record_string(record, "eval_name"),
                "language": record_string(record, "language"),
                "task_type": record_string(record, "task_type"),
                "messages": context.clone(),
                "tools": tools.clone(),
                "assistant_tool_text": message.content,
                "metadata": metadata.clone(),
            }));
        }
        // The served conversation carried tool results on the text channel as
        // ordinary user turns; replay that rendering from the canonical row.
        let served_role = if message.role == "tool" {
            "user"
        } else {
            message.role.as_str()
        };
        context.push(normalized_message(served_role, &message.content));
    }
    if rows.is_empty() {
        return Err("projected example has no assistant tool-call turn to train on".to_string());
    }
    Ok(ConvertedExport {
        rows,
        tool_calls,
        tool_results,
        behavior_classes: behavior_classes.clone(),
    })
}

/// Carry the projection's provenance onto every exported row, so a trained
/// adapter can be traced back to the exact run and transcript bytes.
fn projected_metadata(
    record: &Map<String, Value>,
    target: &ExportTarget,
    dataset_format: &str,
    example: &harn_vm::orchestration::AgentTrainingExample,
    tools: &[Value],
    system_text: &str,
) -> Value {
    let mut metadata = export_metadata(
        record,
        target,
        "harn_projected_run_v1",
        dataset_format,
        tools,
        system_text,
        &BTreeSet::new(),
    );
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "source_projection".to_string(),
            serde_json::to_value(&example.provenance).unwrap_or(Value::Null),
        );
        object.insert(
            "tool_catalog_source".to_string(),
            Value::String("projected".to_string()),
        );
    }
    metadata
}
