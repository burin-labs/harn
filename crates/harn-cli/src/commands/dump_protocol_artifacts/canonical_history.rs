//! Project canonical history positions from their owning VM schema.

use super::records::{FieldKind, Integer, Target};
use super::schema_records::SchemaRecords;

pub(super) fn schema() -> serde_json::Value {
    harn_vm::agent_sessions::canonical_history_boundaries_schema()
}

pub(super) fn append(out: &mut String, target: Target) {
    let mut schema = schema();
    let root = schema.clone();
    schema["$defs"]["CanonicalHistoryBoundaries"] = root;
    let names = [
        (
            "CanonicalSessionBoundary",
            "HarnCanonicalSessionBoundary".into(),
        ),
        (
            "CanonicalHistoryPosition",
            "HarnCanonicalHistoryPosition".into(),
        ),
        (
            "CanonicalHistoryBoundaries",
            "HarnCanonicalHistoryBoundaries".into(),
        ),
    ];
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "canonical history boundary",
        require_all: false,
        metadata: |_, field, _| {
            Ok(match field {
                "event_id" => Some(FieldKind::Nullable(Box::new(FieldKind::Integer(
                    Integer::UnsignedHarn,
                )))),
                _ => None,
            })
        },
    }
    .load()
    .expect("canonical history schema projects to host records");
    for record in records {
        record.append(out, target);
    }
}
