//! Recap write-schema fields projected into the shared host record model.

use super::records::{snake_ident, Field, FieldKind, Integer, Record, Target};
use serde_json::Value;
use std::collections::BTreeSet;

const RECORDS: &[(&str, &str)] = &[
    ("query", "Query"),
    ("cursor", "Cursor"),
    ("coverage", "Coverage"),
    ("sourceEvent", "SourceEvent"),
    ("source", "Source"),
    ("textFact", "TextFact"),
    ("verification", "VerificationFact"),
    ("toolExchange", "ToolExchange"),
    ("planStep", "PlanStep"),
    ("planEvent", "PlanEventFact"),
    ("planFact", "PlanFact"),
    ("progressEntry", "ProgressEntry"),
    ("progressFact", "ProgressFact"),
    ("terminalFact", "TerminalFact"),
    ("iteration", "Iteration"),
    ("turn", "PromptTurnRecap"),
    ("snapshot", "Snapshot"),
];

const ENUMS: &[(&str, &str)] = &[
    ("CompletionState", "/$defs/completionState/enum"),
    ("ToolState", "/$defs/toolExchange/properties/state/enum"),
    ("PlanStepStatus", "/$defs/planStep/properties/status/enum"),
    ("PlanEventKind", "/$defs/planEvent/properties/kind/enum"),
    (
        "ProgressStatus",
        "/$defs/progressEntry/properties/status/enum",
    ),
    (
        "ProgressPriority",
        "/$defs/progressEntry/properties/priority/oneOf/0/enum",
    ),
    (
        "VerificationStatus",
        "/$defs/verification/properties/status/const",
    ),
    ("UnavailableReason", "/oneOf/1/properties/reason/enum"),
];

pub(super) fn append_enums(out: &mut String, target: Target) {
    let schema = harn_vm::session_recap::session_recap_json_schema();
    for (suffix, pointer) in ENUMS {
        let value = schema
            .pointer(pointer)
            .expect("registered recap vocabulary exists");
        let values = if let Some(values) = value.as_array() {
            values.clone()
        } else {
            vec![value.clone()]
        };
        let values = values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("recap vocabulary is strings")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        append_enum(out, target, suffix, &values);
    }
    if matches!(target, Target::Swift | Target::Python) {
        let values = schema["oneOf"]
            .as_array()
            .expect("recap availability variants")
            .iter()
            .map(|variant| {
                variant["properties"]["state"]["const"]
                    .as_str()
                    .expect("recap availability discriminator")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        append_enum(out, target, "AvailabilityState", &values);
    }
}

fn append_enum(out: &mut String, target: Target, suffix: &str, values: &[String]) {
    let name = format!("HarnSessionRecap{suffix}");
    match target {
        Target::Typescript => out.push_str(&format!("export type {name} = {}\n", values.iter().map(|value| format!("{value:?}")).collect::<Vec<_>>().join(" | "))),
        Target::Rust => out.push_str(&format!("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = \"snake_case\")]\npub enum {name} {{ {} }}\n", values.iter().map(|value| super::rust::rust_type_name(value)).collect::<Vec<_>>().join(", "))),
        Target::Swift => out.push_str(&format!("public enum {name}: String, Codable, Sendable, Equatable {{ case {} }}\n", values.iter().map(|value| {
            let case = super::swift::swift_case_name(value);
            if &case == value { case } else { format!("{case} = {value:?}") }
        }).collect::<Vec<_>>().join(", "))),
        Target::Python => out.push_str(&super::python::py_str_enum_owned(&name, values)),
        Target::Go => out.push_str(&format!("type {name} string\n")),
    }
}

fn record_name(key: &str) -> Option<String> {
    RECORDS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, suffix)| {
            if key == "turn" {
                "HarnSessionPromptTurnRecap".into()
            } else {
                format!("HarnSessionRecap{suffix}")
            }
        })
}

pub(super) fn load() -> Result<Vec<Record>, String> {
    from_schema(&harn_vm::session_recap::session_recap_json_schema())
}

fn from_schema(schema: &Value) -> Result<Vec<Record>, String> {
    let definitions = &schema["$defs"];
    RECORDS
        .iter()
        .map(|(key, _)| {
            let object = &definitions[key];
            if object["additionalProperties"] != false {
                return Err(format!("recap {key} must be closed"));
            }
            let properties = object["properties"]
                .as_object()
                .ok_or_else(|| format!("missing recap {key} properties"))?;
            let required = object["required"]
                .as_array()
                .ok_or_else(|| format!("missing recap {key} required fields"))?;
            let names = required
                .iter()
                .map(|name| name.as_str().ok_or("recap field name must be a string"))
                .collect::<Result<BTreeSet<_>, _>>()?;
            if names.len() != required.len() {
                return Err(format!("recap {key} repeats a required field"));
            }
            if names != properties.keys().map(String::as_str).collect() {
                return Err(format!(
                    "recap {key} requires explicit presence for every field"
                ));
            }
            let fields = required
                .iter()
                .map(|field| {
                    let name = field.as_str().ok_or("recap field name must be a string")?;
                    let shape = properties
                        .get(name)
                        .ok_or_else(|| format!("missing recap {key}.{name}"))?;
                    Ok(Field {
                        wire_name: name.to_owned().into(),
                        rust_name: snake_ident(name).into(),
                        kind: kind(definitions, key, name, shape, &mut BTreeSet::new())?,
                        required: true,
                        identity: false,
                    })
                })
                .collect::<Result<_, String>>()?;
            Ok(Record {
                name: record_name(key).expect("registered recap record"),
                fields,
            })
        })
        .collect()
}

fn kind(
    definitions: &Value,
    owner: &str,
    field: &str,
    schema: &Value,
    references: &mut BTreeSet<String>,
) -> Result<FieldKind, String> {
    if let Some(reference) = schema["$ref"].as_str() {
        let key = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| format!("unsupported recap reference {reference}"))?;
        if let Some(name) = record_name(key) {
            return Ok(FieldKind::Named(name));
        }
        let definition = definitions
            .get(key)
            .ok_or_else(|| format!("missing recap reference {reference}"))?;
        if !references.insert(key.to_owned()) {
            return Err(format!("cyclic recap reference {reference}"));
        }
        let result = kind(definitions, owner, field, definition, references);
        references.remove(key);
        return result;
    }
    if let Some(variants) = schema["oneOf"].as_array() {
        if variants.len() == 2 && variants[1]["type"] == "null" {
            return Ok(FieldKind::Nullable(Box::new(kind(
                definitions,
                owner,
                field,
                &variants[0],
                references,
            )?)));
        }
    }
    if let Some(types) = schema["type"].as_array() {
        if types.len() == 2 && types[1] == "null" {
            let mut inner = schema.clone();
            inner["type"] = types[0].clone();
            return Ok(FieldKind::Nullable(Box::new(kind(
                definitions,
                owner,
                field,
                &inner,
                references,
            )?)));
        }
    }
    if schema.get("enum").is_some() || schema["const"].is_string() {
        let suffix = match (owner, field) {
            ("toolExchange", "state") => "ToolState",
            ("planStep", "status") => "PlanStepStatus",
            ("planEvent", "kind") => "PlanEventKind",
            ("progressEntry", "status") => "ProgressStatus",
            ("progressEntry", "priority") => "ProgressPriority",
            ("verification", "status") => "VerificationStatus",
            (_, "state") => "CompletionState",
            _ => return Err(format!("unmapped recap enum {owner}.{field}")),
        };
        return Ok(FieldKind::Named(format!("HarnSessionRecap{suffix}")));
    }
    Ok(match schema["type"].as_str() {
        Some("string") => FieldKind::String,
        Some("boolean") => FieldKind::Bool,
        Some("integer") | None if schema["type"] == "integer" || schema["const"].is_u64() => {
            FieldKind::Integer(match (owner, field) {
                ("snapshot", "schemaVersion") => Integer::U32,
                ("coverage", _) | ("query", "limit") => Integer::Usize,
                ("iteration", "iteration") => Integer::I64,
                _ => Integer::U64,
            })
        }
        Some("array") => FieldKind::List(Box::new(kind(
            definitions,
            owner,
            field,
            &schema["items"],
            references,
        )?)),
        Some("object") if schema["additionalProperties"] == true => FieldKind::JsonObject,
        None if schema.as_object().is_some_and(|object| object.is_empty()) => {
            FieldKind::Nullable(Box::new(FieldKind::Json))
        }
        _ => return Err(format!("unsupported recap shape {owner}.{field}: {schema}")),
    })
}

pub(super) fn append(out: &mut String, target: Target) {
    let records = load().expect("the owning recap schema projects to host records");
    for record in &records {
        record.append_closed(out, target);
        if matches!(target, Target::Python) {
            let conversions = record
                .fields
                .iter()
                .filter_map(|field| {
                    let value = format!("values[{:?}]", field.wire_name);
                    python_value(&field.kind, &value, &records)
                        .map(|conversion| format!("        {value} = {conversion}\n"))
                })
                .collect::<String>();
            if !conversions.is_empty() {
                out.push_str(&format!("    @classmethod\n    def from_wire(cls, data: Mapping[str, Any]) -> {:?}:\n        values = cls._strict_values(data)\n{conversions}        return cls(**values)\n\n", record.name));
            }
        }
    }
}

fn python_value(kind: &FieldKind, value: &str, records: &[Record]) -> Option<String> {
    match kind {
        FieldKind::Named(name) => Some(if records.iter().any(|record| &record.name == name) {
            format!("{name}.from_wire({value})")
        } else {
            format!("{name}({value})")
        }),
        FieldKind::List(inner) => python_value(inner, "item", records)
            .map(|expression| format!("[{expression} for item in {value}]")),
        FieldKind::Nullable(inner) => python_value(inner, value, records)
            .map(|expression| format!("None if {value} is None else {expression}")),
        _ => None,
    }
}

pub(super) fn append_validators(out: &mut String, target: Target) {
    let records = load().expect("the owning recap schema projects to host records");
    for record in &records {
        let name = &record.name;
        let keys = record
            .fields
            .iter()
            .map(|field| format!("{:?}", field.wire_name))
            .collect::<Vec<_>>()
            .join(", ");
        let mut body = String::new();
        for field in &record.fields {
            let value = format!("object[{:?}]", field.wire_name);
            body.push_str(&validation(&field.kind, &value, &records, target));
        }
        if name == "HarnSessionRecapVerificationFact" && matches!(target, Target::Typescript) {
            let schema = harn_vm::session_recap::session_recap_json_schema();
            let status = schema["$defs"]["verification"]["properties"]["status"]["const"]
                .as_str()
                .expect("verification status literal");
            body.push_str(&format!("  if (object.status !== {status:?}) throw new TypeError(\"Harn session recap verification status must be {status}\")\n"));
        }
        match target {
            Target::Typescript => {
                let binding = if body.is_empty() {
                    ""
                } else {
                    "const object = "
                };
                out.push_str(&format!("function validate{name}(value: unknown): void {{\n  {binding}harnSessionRecapObject(value, {name:?}, [{keys}])\n"));
            }
            Target::Swift => {
                let binding = if body.is_empty() { "_" } else { "let object" };
                out.push_str(&format!("private func validate{name}(_ value: HarnACPValue?) throws {{\n    {binding} = try harnSessionRecapObject(value, label: {name:?}, keys: [{keys}])\n"));
            }
            _ => unreachable!("only Swift and TypeScript need an explicit validation walk"),
        }
        out.push_str(&body);
        out.push_str("}\n\n");
    }
}

fn validation(kind: &FieldKind, value: &str, records: &[Record], target: Target) -> String {
    match kind {
        FieldKind::Named(name) if records.iter().any(|record| &record.name == name) => match target
        {
            Target::Typescript => format!("  validate{name}({value})\n"),
            Target::Swift => format!("    try validate{name}({value})\n"),
            _ => unreachable!(),
        },
        FieldKind::Nullable(inner) => {
            let body = validation(inner, value, records, target);
            if body.is_empty() {
                return body;
            }
            match target {
                Target::Typescript => format!("  if ({value} !== null) {{\n{body}  }}\n"),
                Target::Swift => format!("    if {value} != .null {{\n{body}    }}\n"),
                _ => unreachable!(),
            }
        }
        FieldKind::List(inner) => {
            let body = validation(inner, "item", records, target);
            if body.is_empty() {
                return body;
            }
            match target {
                Target::Typescript => format!("  for (const item of harnSessionRecapArray({value}, {value:?})) {{\n{body}  }}\n"),
                Target::Swift => format!("    for item in try harnSessionRecapArray({value}, label: {value:?}) {{\n{body}    }}\n"),
                _ => unreachable!(),
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_inventory_and_recursive_aliases() {
        let mut schema = harn_vm::session_recap::session_recap_json_schema();
        schema["$defs"]["query"]["required"][1] = Value::String("sessionId".into());
        assert_eq!(
            from_schema(&schema).unwrap_err(),
            "recap query repeats a required field"
        );
        schema = harn_vm::session_recap::session_recap_json_schema();
        schema["$defs"]["nullableString"] = serde_json::json!({"$ref": "#/$defs/nullableString"});
        assert_eq!(
            from_schema(&schema).unwrap_err(),
            "cyclic recap reference #/$defs/nullableString"
        );
    }
}
