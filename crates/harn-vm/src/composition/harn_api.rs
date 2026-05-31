use std::collections::BTreeSet;

use serde_json::Value;

use super::manifest::{BindingManifest, BindingPolicyDisposition};

pub fn composition_harn_api(manifest: &BindingManifest) -> String {
    let mut out = String::from("// Harn Code Mode API. Snippets call these bindings; the executor supplies __composition_call.\n");
    out.push_str("type JsonValue = unknown\n\n");
    for binding in &manifest.bindings {
        if binding.policy.disposition != BindingPolicyDisposition::Allowed {
            continue;
        }
        let args_type_name = unique_type_name(&binding.binding, "Args");
        let args_type = json_schema_to_harn_type(&binding.input_schema);
        let result_type = binding
            .output_schema
            .as_ref()
            .map(json_schema_to_harn_type)
            .unwrap_or_else(|| "JsonValue".to_string());
        if let Some(server) = binding
            .metadata
            .get("_mcp_server")
            .or_else(|| binding.metadata.get("mcp_server"))
            .and_then(Value::as_str)
        {
            let tool = binding
                .metadata
                .get("_mcp_tool_name")
                .and_then(Value::as_str)
                .unwrap_or(&binding.name);
            out.push_str(&format!("// MCP {server}/{tool} -> {}\n", binding.binding));
        } else if binding.source != "harn" {
            out.push_str(&format!("// {} -> {}\n", binding.source, binding.binding));
        }
        out.push_str(&format!("type {args_type_name} = {args_type}\n"));
        out.push_str(&format!(
            "fn {}(args: {args_type_name}) -> {result_type} {{\n  return __composition_call({}, args)\n}}\n\n",
            binding.binding,
            harn_string_literal(&binding.name),
        ));
    }
    out
}

fn unique_type_name(binding: &str, suffix: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for ch in binding.chars() {
        if ch == '_' || ch == '-' || ch == '.' || ch == '/' {
            upper_next = true;
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            if out.is_empty() && ch.is_ascii_digit() {
                out.push_str("Tool");
            }
            if upper_next {
                out.push(ch.to_ascii_uppercase());
                upper_next = false;
            } else {
                out.push(ch);
            }
        }
    }
    if out.is_empty() {
        out.push_str("Tool");
    }
    out.push_str(suffix);
    out
}

fn json_schema_to_harn_type(schema: &Value) -> String {
    if let Some(shorthand) = schema.as_str() {
        return match shorthand {
            "string" => "string".to_string(),
            "int" | "integer" => "int".to_string(),
            "float" | "number" => "float".to_string(),
            "bool" | "boolean" => "bool".to_string(),
            "list" | "array" => "list<JsonValue>".to_string(),
            "dict" | "object" => "dict<string, JsonValue>".to_string(),
            _ => "JsonValue".to_string(),
        };
    }
    let schema_type = schema.get("type").and_then(Value::as_str);
    match schema_type {
        Some("string") => enum_string_literals(schema).unwrap_or_else(|| "string".to_string()),
        Some("integer") => "int".to_string(),
        Some("number") => "float".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("array") => {
            let item_type = schema
                .get("items")
                .map(json_schema_to_harn_type)
                .unwrap_or_else(|| "JsonValue".to_string());
            format!("list<{item_type}>")
        }
        Some("object") | None if schema.get("properties").is_some() => object_shape(schema),
        None if schema.as_object().is_some() => object_shape_from_shorthand(schema),
        Some("object") => "dict<string, JsonValue>".to_string(),
        _ => "JsonValue".to_string(),
    }
}

fn object_shape(schema: &Value) -> String {
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
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "dict<string, JsonValue>".to_string();
    };
    let mut fields = Vec::new();
    for (name, value) in properties {
        if !is_harn_identifier(name) {
            return "dict<string, JsonValue>".to_string();
        }
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        fields.push(format!(
            "{name}{optional}: {}",
            json_schema_to_harn_type(value)
        ));
    }
    if fields.is_empty() {
        "dict<string, JsonValue>".to_string()
    } else {
        format!("{{{}}}", fields.join(", "))
    }
}

fn object_shape_from_shorthand(schema: &Value) -> String {
    let Some(properties) = schema.as_object() else {
        return "dict<string, JsonValue>".to_string();
    };
    let mut fields = Vec::new();
    for (name, value) in properties {
        if !is_harn_identifier(name) {
            return "dict<string, JsonValue>".to_string();
        }
        let optional = if value
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            ""
        } else {
            "?"
        };
        fields.push(format!(
            "{name}{optional}: {}",
            json_schema_to_harn_type(value)
        ));
    }
    if fields.is_empty() {
        "dict<string, JsonValue>".to_string()
    } else {
        format!("{{{}}}", fields.join(", "))
    }
}

fn enum_string_literals(schema: &Value) -> Option<String> {
    let variants = schema.get("enum")?.as_array()?;
    let strings = variants
        .iter()
        .map(|value| value.as_str().map(harn_string_literal))
        .collect::<Option<Vec<_>>>()?;
    (!strings.is_empty()).then(|| strings.join(" | "))
}

fn is_harn_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '_' && !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) && !HARN_KEYWORDS.contains(&name)
}

fn harn_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

const HARN_KEYWORDS: &[&str] = &[
    "agent",
    "as",
    "await",
    "break",
    "catch",
    "continue",
    "defer",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "import",
    "in",
    "interface",
    "let",
    "match",
    "nil",
    "pipeline",
    "pub",
    "return",
    "skill",
    "spawn",
    "struct",
    "throw",
    "true",
    "try",
    "type",
    "var",
    "while",
];
