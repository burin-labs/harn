use std::collections::BTreeSet;

use serde_json::Value;

use super::manifest::{BindingManifest, BindingPolicyDisposition};

pub fn composition_typescript_declarations(manifest: &BindingManifest) -> String {
    let mut out = String::from(
        "export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };\n",
    );
    out.push_str("export type CompositionToolResult = JsonValue;\n\n");
    for binding in &manifest.bindings {
        if binding.policy.disposition != BindingPolicyDisposition::Allowed {
            continue;
        }
        let args_type = json_schema_to_typescript(&binding.input_schema);
        let result_type = binding
            .output_schema
            .as_ref()
            .map(json_schema_to_typescript)
            .unwrap_or_else(|| "CompositionToolResult".to_string());
        out.push_str(&format!(
            "export declare function {}(args: {}): Promise<{}>;\n",
            binding.binding, args_type, result_type
        ));
    }
    out
}

fn json_schema_to_typescript(schema: &Value) -> String {
    if let Some(shorthand) = schema.as_str() {
        return match shorthand {
            "string" => "string".to_string(),
            "int" | "integer" | "float" | "number" => "number".to_string(),
            "bool" | "boolean" => "boolean".to_string(),
            "list" | "array" => "JsonValue[]".to_string(),
            "dict" | "object" => "{ [key: string]: JsonValue }".to_string(),
            _ => "JsonValue".to_string(),
        };
    }
    let schema_type = schema.get("type").and_then(Value::as_str);
    match schema_type {
        Some("string") => enum_string_literals(schema).unwrap_or_else(|| "string".to_string()),
        Some("integer") | Some("number") => "number".to_string(),
        Some("boolean") => "boolean".to_string(),
        Some("array") => {
            let item_type = schema
                .get("items")
                .map(json_schema_to_typescript)
                .unwrap_or_else(|| "JsonValue".to_string());
            format!("{item_type}[]")
        }
        Some("object") | None if schema.get("properties").is_some() => {
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let mut fields = Vec::new();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, value) in properties {
                    let marker = if required.contains(name.as_str()) {
                        ""
                    } else {
                        "?"
                    };
                    fields.push(format!(
                        "{}{}: {}",
                        typescript_property_name(name),
                        marker,
                        json_schema_to_typescript(value)
                    ));
                }
            }
            if fields.is_empty() {
                "{ [key: string]: JsonValue }".to_string()
            } else {
                format!("{{ {} }}", fields.join("; "))
            }
        }
        None if schema.as_object().is_some() => {
            let fields = schema
                .as_object()
                .into_iter()
                .flat_map(|properties| properties.iter())
                .map(|(name, value)| {
                    let marker = if value
                        .get("required")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                    {
                        ""
                    } else {
                        "?"
                    };
                    format!(
                        "{}{}: {}",
                        typescript_property_name(name),
                        marker,
                        json_schema_to_typescript(value)
                    )
                })
                .collect::<Vec<_>>();
            if fields.is_empty() {
                "{ [key: string]: JsonValue }".to_string()
            } else {
                format!("{{ {} }}", fields.join("; "))
            }
        }
        Some("object") => "{ [key: string]: JsonValue }".to_string(),
        _ => "JsonValue".to_string(),
    }
}

fn enum_string_literals(schema: &Value) -> Option<String> {
    let variants = schema.get("enum")?.as_array()?;
    let strings = variants
        .iter()
        .map(|value| value.as_str().map(|text| format!("{text:?}")))
        .collect::<Option<Vec<_>>>()?;
    (!strings.is_empty()).then(|| strings.join(" | "))
}

fn typescript_property_name(name: &str) -> String {
    if name.chars().enumerate().all(|(idx, ch)| {
        ch == '_' || ch.is_ascii_alphanumeric() && (idx > 0 || !ch.is_ascii_digit())
    }) {
        name.to_string()
    } else {
        format!("{name:?}")
    }
}
