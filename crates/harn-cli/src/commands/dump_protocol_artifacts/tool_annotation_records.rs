//! Tool metadata projected from the execution policy's serialized contract.

use super::records::{FieldKind, RecordOptions, Target};
use super::schema_records::SchemaRecords;

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_vm::tool_annotations::ToolAnnotations::json_schema();
    let names = [
        ("ToolArgSchema", "HarnToolArgSchema".into()),
        ("", "HarnToolAnnotations".into()),
    ];
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "tool annotations",
        require_all: false,
        metadata: |_, field, shape| {
            Ok(match field {
                // Keep the host bindings' string-keyed range representation.
                "dependency_range_params" => Some(FieldKind::List(Box::new(FieldKind::Map(
                    Box::new(FieldKind::String),
                )))),
                _ => match shape["$ref"].as_str() {
                    Some("#/$defs/ToolKind") => Some(FieldKind::Named("HarnACPToolKind".into())),
                    Some("#/$defs/SideEffectLevel") => {
                        Some(FieldKind::Named("HarnSideEffectLevel".into()))
                    }
                    Some("#/$defs/CompletionEvidenceRole") => {
                        Some(FieldKind::Named("HarnCompletionEvidenceRole".into()))
                    }
                    _ => None,
                },
            })
        },
    }
    .load_extensible()
    .expect("tool annotations project to host bindings");
    for mut record in records {
        let mut defaults = if record.name == "HarnToolArgSchema" {
            schema["$defs"]["ToolArgSchema"].clone()
        } else {
            schema.clone()
        };
        if record.name == "HarnToolAnnotations" {
            for (name, shape) in defaults["properties"].as_object_mut().unwrap() {
                if !matches!(
                    name.as_str(),
                    "capabilities" | "emits_artifacts" | "result_readers" | "inline_result"
                ) {
                    shape.as_object_mut().unwrap().remove("default");
                }
            }
        }
        for field in &mut record.fields {
            if let FieldKind::Named(name) = &mut field.kind {
                match (target, name.as_str()) {
                    (Target::Python, "HarnACPToolKind" | "HarnSideEffectLevel") => {
                        *name = "str".into();
                    }
                    (Target::Go, "HarnACPToolKind") => *name = "ACPToolKind".into(),
                    _ => {}
                }
            }
            if matches!(target, Target::Swift) && matches!(field.kind, FieldKind::Nullable(_)) {
                field.required = false;
            }
        }
        record.append_record(
            out,
            target,
            RecordOptions {
                mutable: true,
                zero_defaults: if matches!(target, Target::Python)
                    || (matches!(target, Target::Rust) && record.name == "HarnToolArgSchema")
                {
                    Some(&defaults)
                } else {
                    None
                },
                ..RecordOptions::default()
            },
        );
    }
}
