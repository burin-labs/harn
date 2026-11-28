//! Timeline bindings projected from the runtime's serialized records.

use super::records::{FieldKind, Integer, RecordOptions, Target};
use super::schema_records::SchemaRecords;
use serde_json::Value;

pub(super) fn round_trip_fixture() -> Result<Value, String> {
    use harn_vm::session_timeline::{SessionTimelineSnapshot, SessionTimelineUpdate};
    let snapshot: SessionTimelineSnapshot = serde_json::from_value(serde_json::json!({
        "schemaVersion": 2,
        "query": {"sessionId": "session", "runId": "run", "runPath": "/run", "projectId": "project", "fromCursor": {"topics": {"events": 1}}, "limit": 4},
        "cursor": {"topics": {"events": 8}},
        "coverage": {"returned": 1, "available": 1, "truncated": false},
        "nodes": [{
            "id": "node", "parentId": "parent", "children": ["child"],
            "category": "tool", "kind": "future_kind", "name": "Verify", "status": "complete",
            "traceId": "trace", "spanId": "span", "occurredAtMs": -1, "startMs": 2, "durationMs": 3,
            "attributes": {"nested": [null, true, 4]}, "order": 8,
            "references": [{"kind": "source", "id": "reference", "topic": "events", "eventId": 8}],
            "links": [{"kind": "cause", "targetId": "target", "traceId": "trace", "spanId": "span", "eventId": "event"}]
        }]
    })).map_err(|error| error.to_string())?;
    let update = SessionTimelineUpdate {
        schema_version: snapshot.schema_version,
        cursor: snapshot.cursor.clone(),
        node: snapshot.nodes[0].clone(),
    };
    Ok(serde_json::json!({"snapshot": snapshot, "update": update}))
}

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_vm::session_timeline::session_timeline_json_schema();
    let names = [
        "Cursor",
        "Query",
        "Reference",
        "Link",
        "Node",
        "Coverage",
        "Snapshot",
        "Update",
    ]
    .map(|suffix| {
        (
            format!("SessionTimeline{suffix}"),
            format!("HarnSessionTimeline{suffix}"),
        )
    });
    let names = names
        .iter()
        .map(|(key, name)| (key.as_str(), name.clone()))
        .collect::<Vec<_>>();
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "session timeline",
        require_all: false,
        metadata: match target {
            Target::Swift => swift_metadata,
            _ => metadata,
        },
    }
    .load_extensible()
    .expect("timeline records project to host bindings");
    for mut record in records {
        for field in &mut record.fields {
            if matches!(field.kind, FieldKind::Nullable(_))
                && (matches!(target, Target::Swift)
                    || (matches!(target, Target::Go | Target::Typescript | Target::Python)
                        && record.name == "HarnSessionTimelineQuery"))
            {
                field.required = false;
            }
            if matches!(target, Target::Go) && matches!(field.kind, FieldKind::Json) {
                field.kind = FieldKind::Nullable(Box::new(FieldKind::Json));
                field.required = false;
            }
        }
        let shape = &schema["$defs"][record.name.strip_prefix("Harn").unwrap()];
        let defaults = match target {
            Target::Rust => Some(shape),
            Target::Python
                if matches!(
                    record.name.as_str(),
                    "HarnSessionTimelineCursor"
                        | "HarnSessionTimelineQuery"
                        | "HarnSessionTimelineNode"
                ) =>
            {
                Some(shape)
            }
            _ => None,
        };
        record.append_record(
            out,
            target,
            RecordOptions {
                mutable: true,
                python_plain: true,
                zero_defaults: defaults,
                ..RecordOptions::default()
            },
        );
    }
    if matches!(target, Target::Swift) {
        out.push_str("extension HarnSessionTimelineNode: Identifiable {}\n\n");
    }
}

fn metadata(_: &str, _: &str, shape: &Value) -> Result<Option<FieldKind>, String> {
    if shape["type"] == "integer" {
        return Ok(Some(FieldKind::Integer(match shape["format"].as_str() {
            Some("uint32") => Integer::U32,
            Some("uint") => Integer::Usize,
            Some("int64") => Integer::I64,
            Some("uint64") => Integer::U64,
            _ => return Err(format!("unsupported timeline integer: {shape}")),
        })));
    }
    Ok((shape == &Value::Bool(true)
        || shape.as_object().is_some_and(|shape| {
            shape
                .keys()
                .all(|key| matches!(key.as_str(), "description" | "title" | "default"))
        }))
    .then_some(FieldKind::Json))
}

fn swift_metadata(owner: &str, field: &str, shape: &Value) -> Result<Option<FieldKind>, String> {
    if shape["type"] != "integer" {
        return metadata(owner, field, shape);
    }
    if let Some(name) = match shape["format"].as_str() {
        Some("uint32") => Some("UInt32"),
        Some("uint64") => Some("UInt64"),
        _ => None,
    } {
        return Ok(Some(FieldKind::Named(name.into())));
    }
    if shape["format"] == "int64" {
        return Ok(Some(FieldKind::Named("Int64".into())));
    }
    metadata(owner, field, shape)
}
