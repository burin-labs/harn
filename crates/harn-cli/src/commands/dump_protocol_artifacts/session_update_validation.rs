//! Compile the schema's scalar and presence constraints into host decoders.

use super::records::Target;
use serde_json::Value;

pub(super) fn append_checks(out: &mut String, shape: &Value, target: Target) {
    append_object(out, shape, &[], target);
}

fn access(path: &[String], target: Target) -> String {
    match target {
        Target::Rust => format!("value.pointer({:?})", format!("/{}", path.join("/"))),
        Target::Swift => {
            let mut result = "value".to_owned();
            for (index, key) in path.iter().enumerate() {
                result.push_str(&format!("{}[{key:?}]", if index == 0 { "" } else { "?" }));
            }
            result
        }
        _ => unreachable!("only native decoders need runtime checks"),
    }
}

fn refuse(out: &mut String, condition: &str, path: &[String], reason: &str, target: Target) {
    let message = format!("session update {} {reason}", path.join("."));
    match target {
        Target::Rust => out.push_str(&format!("                if {condition} {{ return Err(serde::de::Error::custom({message:?})); }}\n")),
        Target::Swift => out.push_str(&format!("            if {condition} {{ throw DecodingError.dataCorruptedError(in: container, debugDescription: {message:?}) }}\n")),
        _ => unreachable!(),
    }
}

fn append_object(out: &mut String, shape: &Value, path: &[String], target: Target) {
    let Some(properties) = shape["properties"].as_object() else {
        return;
    };
    for (key, field) in properties {
        let mut child_path = path.to_vec();
        child_path.push(key.clone());
        let value = access(&child_path, target);
        if shape["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|name| name == key))
        {
            let missing = match target {
                Target::Rust => format!("{value}.is_none()"),
                Target::Swift => format!("{value} == nil"),
                _ => unreachable!(),
            };
            refuse(out, &missing, &child_path, "is required", target);
        }
        if let Some(minimum) = field["minLength"].as_u64() {
            let too_short = match target {
                Target::Rust => format!(
                    "{value}.and_then(Value::as_str).is_some_and(|text| text.chars().count() < {minimum})"
                ),
                Target::Swift => {
                    format!("({value}?.stringValue?.unicodeScalars.count ?? {minimum}) < {minimum}")
                }
                _ => unreachable!(),
            };
            refuse(out, &too_short, &child_path, "is too short", target);
        }
        if let Some(minimum) = field["minimum"].as_i64() {
            let too_small = match target {
                Target::Rust => format!(
                    "{value}.and_then(Value::as_i64).is_some_and(|number| number < {minimum})"
                ),
                Target::Swift => format!("({value}?.intValue ?? {minimum}) < {minimum}"),
                _ => unreachable!(),
            };
            refuse(out, &too_small, &child_path, "is below its minimum", target);
        }
        if let Some(pattern) = field["pattern"].as_str() {
            assert_eq!(pattern, "\\S", "unsupported session-update string pattern");
            let blank = match target {
                Target::Rust => format!(
                    "{value}.and_then(Value::as_str).is_some_and(|text| text.trim().is_empty())"
                ),
                Target::Swift => format!(
                    "{value}?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true"
                ),
                _ => unreachable!(),
            };
            refuse(out, &blank, &child_path, "must not be blank", target);
        }
        if let Some(choices) = field["enum"].as_array() {
            let choices = string_choices(choices);
            let outside = match target {
                Target::Rust => format!(
                    "{value}.and_then(Value::as_str).is_some_and(|text| ![{choices}].contains(&text))"
                ),
                Target::Swift => {
                    format!("{value}?.stringValue.map({{ ![{choices}].contains($0) }}) == true")
                }
                _ => unreachable!(),
            };
            refuse(out, &outside, &child_path, "has an unknown value", target);
        }
        if field["properties"].is_object() {
            let mut checks = String::new();
            append_object(&mut checks, field, &child_path, target);
            if checks.is_empty() {
                continue;
            }
            let present = match target {
                Target::Rust => format!("{value}.is_some()"),
                Target::Swift => format!("{value} != nil"),
                _ => unreachable!(),
            };
            out.push_str(&format!("            if {present} {{\n"));
            out.push_str(&checks);
            out.push_str("            }\n");
        }
    }
    if let Some(conditions) = shape["allOf"].as_array() {
        for rule in conditions {
            let condition = rule["if"]["properties"]
                .as_object()
                .expect("conditional presence properties");
            assert_eq!(
                condition.len(),
                1,
                "conditional presence has one discriminator"
            );
            let (key, condition) = condition.iter().next().unwrap();
            let mut condition_path = path.to_vec();
            condition_path.push(key.clone());
            let value = access(&condition_path, target);
            let choices = string_choices(
                condition["enum"]
                    .as_array()
                    .expect("conditional string enum"),
            );
            let applies = match target {
                Target::Rust => format!(
                    "{value}.and_then(Value::as_str).is_some_and(|text| [{choices}].contains(&text))"
                ),
                Target::Swift => {
                    format!("{value}?.stringValue.map({{ [{choices}].contains($0) }}) == true")
                }
                _ => unreachable!(),
            };
            for field in rule["then"]["required"]
                .as_array()
                .expect("conditional required fields")
            {
                let mut required_path = path.to_vec();
                required_path.push(field.as_str().expect("field name").to_owned());
                let required = access(&required_path, target);
                let missing = match target {
                    Target::Rust => format!("{applies} && {required}.is_none()"),
                    Target::Swift => format!("({applies}) && {required} == nil"),
                    _ => unreachable!(),
                };
                refuse(
                    out,
                    &missing,
                    &required_path,
                    "is required for this phase",
                    target,
                );
            }
            if rule["then"]["properties"].is_object() {
                out.push_str(&format!("            if {applies} {{\n"));
                append_object(out, &rule["then"], path, target);
                out.push_str("            }\n");
            }
        }
    }
}

fn string_choices(choices: &[Value]) -> String {
    choices
        .iter()
        .map(|choice| format!("{:?}", choice.as_str().expect("string enum")))
        .collect::<Vec<_>>()
        .join(", ")
}
