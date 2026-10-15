//! Reject JSON Schema shapes proven to admit no value before they reach a provider.
//!
//! llama.cpp converts an empty combinator (`anyOf: []`, `oneOf: []`,
//! `enum: []`) into `root ::=` and 500s the request. Harn must refuse those
//! schemas at the normalize/emit seam, with a diagnostic that names the tool
//! and the collapsed branch set when either is known. This deliberately
//! sound-but-incomplete analysis skips constraints whose interactions require
//! a full JSON Schema solver.

use serde_json::{Map, Value};

/// A structured-output schema that cannot be satisfied by any JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsatisfiableJsonSchema {
    pub tool: Option<String>,
    pub path: String,
    pub keyword: &'static str,
    pub collapsed_branches: Vec<String>,
}

impl std::fmt::Display for UnsatisfiableJsonSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tool = self.tool.as_deref().unwrap_or("<unknown>");
        let branches = if self.collapsed_branches.is_empty() {
            String::new()
        } else {
            self.collapsed_branches.join(", ")
        };
        let path = &self.path;
        let keyword = self.keyword;
        write!(
            f,
            "structured output schema admits no value: tool `{tool}` collapsed branch set [{branches}] at {path} (`{keyword}`)"
        )
    }
}

/// Fail when `schema` cannot be satisfied by any value.
pub(crate) fn reject_unsatisfiable_output_schema(
    schema: &Value,
) -> Result<(), UnsatisfiableJsonSchema> {
    match first_unsatisfiable_node(schema, "#", None) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn first_unsatisfiable_node(
    value: &Value,
    path: &str,
    inherited_tool: Option<&str>,
) -> Option<UnsatisfiableJsonSchema> {
    let object = match value {
        Value::Bool(false) => {
            return Some(UnsatisfiableJsonSchema {
                tool: inherited_tool.map(str::to_string),
                path: path.to_string(),
                keyword: "false",
                collapsed_branches: Vec::new(),
            });
        }
        Value::Object(object) => object,
        _ => return None,
    };
    let tool = tool_name_from_object(object).or_else(|| inherited_tool.map(str::to_string));
    let tool_ref = tool.as_deref();

    if object
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return Some(UnsatisfiableJsonSchema {
            tool,
            path: child_path(path, "enum"),
            keyword: "enum",
            collapsed_branches: collapsed_branches_from_object(object),
        });
    }

    if schema_requires_object(object) {
        if let Some(missing) = impossible_required_properties(object) {
            return Some(UnsatisfiableJsonSchema {
                tool: tool.clone(),
                path: child_path(path, "required"),
                keyword: "required",
                collapsed_branches: missing,
            });
        }

        if let (Some(Value::Object(properties)), Some(Value::Array(required))) =
            (object.get("properties"), object.get("required"))
        {
            let properties_path = child_path(path, "properties");
            for name in required.iter().filter_map(Value::as_str) {
                if let Some(error) = properties.get(name).and_then(|schema| {
                    first_unsatisfiable_node(schema, &child_path(&properties_path, name), tool_ref)
                }) {
                    return Some(error);
                }
            }
        }
    }

    if let Some(Value::Array(children)) = object.get("allOf") {
        let array_path = child_path(path, "allOf");
        if let Some(error) = children.iter().enumerate().find_map(|(index, schema)| {
            first_unsatisfiable_node(schema, &index_path(&array_path, index), tool_ref)
        }) {
            return Some(error);
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let Some(Value::Array(children)) = object.get(keyword) else {
            continue;
        };
        let array_path = child_path(path, keyword);
        let all_branches_unsatisfiable = children.is_empty()
            || children.iter().enumerate().all(|(index, schema)| {
                first_unsatisfiable_node(schema, &index_path(&array_path, index), tool_ref)
                    .is_some()
            });
        if all_branches_unsatisfiable {
            return Some(UnsatisfiableJsonSchema {
                tool: tool.clone(),
                path: array_path,
                keyword,
                collapsed_branches: collapsed_branches_from_object(object),
            });
        }
    }
    None
}

/// Object-only keywords cannot make a schema unsatisfiable while a non-object
/// value remains admissible. Deliberately require an explicit object-only type;
/// cross-branch inference belongs in a fuller schema solver.
fn schema_requires_object(object: &Map<String, Value>) -> bool {
    match object.get("type") {
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => {
            !kinds.is_empty() && kinds.iter().all(|kind| kind.as_str() == Some("object"))
        }
        _ => false,
    }
}

fn tool_name_from_object(object: &Map<String, Value>) -> Option<String> {
    let name = object.get("properties")?.get("name")?;
    if let Some(value) = name.get("const").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    name.get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn collapsed_branches_from_object(object: &Map<String, Value>) -> Vec<String> {
    object
        .get("x-harn-collapsed-branches")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn impossible_required_properties(object: &Map<String, Value>) -> Option<Vec<String>> {
    if object.get("additionalProperties") != Some(&Value::Bool(false)) {
        return None;
    }
    let required = object.get("required").and_then(Value::as_array)?;
    let required: Vec<String> = required
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if required.is_empty() {
        return None;
    }
    let properties = object.get("properties").and_then(Value::as_object);
    let missing: Vec<String> = match properties {
        Some(properties) => required
            .into_iter()
            .filter(|name| !properties.contains_key(name))
            .collect(),
        None => required,
    };
    (!missing.is_empty()).then_some(missing)
}

fn child_path(parent: &str, child: &str) -> String {
    format!("{parent}/{child}")
}

fn index_path(parent: &str, index: usize) -> String {
    format!("{parent}/{index}")
}
