//! Host records and enums projected from the VM's admission types.

use serde_json::{json, Map, Value};

use super::records::{FieldKind, Target};
use super::schema_records::SchemaRecords;

pub(super) fn schemas() -> [(&'static str, Value); 2] {
    harn_vm::llm::api::inference_admission_schemas()
}

pub(super) fn append(out: &mut String, target: Target) {
    let mut definitions = Map::new();
    for (name, schema) in schemas() {
        if let Some(nested) = schema["$defs"].as_object() {
            for (key, value) in nested {
                if let Some(existing) = definitions.insert(key.clone(), value.clone()) {
                    assert_eq!(existing, *value, "admission schemas must share definitions");
                }
            }
        }
        definitions.insert(name.to_owned(), schema);
    }
    for (key, name) in [
        ("InferenceAdmissionStatus", "HarnInferenceAdmissionStatus"),
        ("InferenceReach", "HarnInferenceAdmissionReach"),
        ("DataPosture", "HarnInferenceAdmissionDataPosture"),
    ] {
        let schema = &definitions[key];
        let values = if let Some(values) = schema["enum"].as_array() {
            values
                .iter()
                .map(|value| value.as_str().expect("string enum").to_owned())
                .collect::<Vec<_>>()
        } else {
            schema["oneOf"]
                .as_array()
                .expect("the owner exports a closed string enum")
                .iter()
                .map(|variant| {
                    variant["const"]
                        .as_str()
                        .expect("string enum variant")
                        .to_owned()
                })
                .collect()
        };
        match target {
            Target::Swift => out.push_str(&super::swift::swift_enum(name, &values)),
            Target::Rust => out.push_str(&super::rust::rust_open_string_enum(
                name,
                "Value-free inference admission vocabulary owned by harn_vm.",
                &values,
            )),
            Target::Typescript => out.push_str(&super::typescript::ts_array_owned(
                &format!(
                    "{}_VALUES",
                    super::records::snake_ident(name).to_uppercase()
                ),
                &values,
                name,
            )),
            Target::Python => out.push_str(&super::python::py_str_enum_owned(name, &values)),
            Target::Go => out.push_str(&super::go::go_typed_array_owned(
                name,
                &format!("{name}Values"),
                &values,
            )),
        }
    }
    let names = [
        ("InferenceBoundary", "HarnInferenceAdmissionBoundary".into()),
        (
            "InferenceAdmissionRequest",
            "HarnInferenceAdmissionRequest".into(),
        ),
        (
            "InferenceAdmissionSnapshot",
            "HarnInferenceAdmissionSnapshot".into(),
        ),
    ];
    let schema = json!({"$defs": definitions});
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "inference admission",
        require_all: false,
        metadata: |_, field, _| {
            Ok(match field {
                "status" => Some(FieldKind::Named("HarnInferenceAdmissionStatus".into())),
                "reach" => Some(FieldKind::Named("HarnInferenceAdmissionReach".into())),
                "data_controls" => {
                    Some(FieldKind::Named("HarnInferenceAdmissionDataPosture".into()))
                }
                _ => None,
            })
        },
    }
    .load_extensible()
    .expect("the owning admission schemas project to host records");
    for record in records {
        record.append(out, target);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
}
