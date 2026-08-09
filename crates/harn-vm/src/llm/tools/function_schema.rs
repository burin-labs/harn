//! Render Harn's canonical tool catalog as provider-shaped function schemas.
//!
//! The transcript sidecar records the catalog as [`ToolSchema`] — Harn's
//! provider-independent form, the same one the prompt renderer and the native
//! tool builders read. Trainers and OpenAI-compatible corpora want the
//! `{"type": "function", "function": {name, description, parameters}}` shape
//! instead.
//!
//! Doing that conversion here, next to `json_schema_to_type_expr` (its
//! inverse), keeps both directions under one owner. The alternative — letting
//! each consumer regenerate a schema from prompt prose or from observed
//! argument values — produces a schema the model was never served, which is
//! exactly the drift the training-example projector exists to prevent.

use serde_json::{Map, Value};

use super::collect::ToolSchema;
use super::params::ToolParamSchema;
use super::type_expr::{ObjectField, TypeExpr};

/// Convert one canonical tool schema into an OpenAI-style function schema.
pub(crate) fn tool_schema_to_function_schema(tool: &ToolSchema) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": params_to_json_schema(&tool.params),
        },
    })
}

/// Convert a serialized `tool_schemas` catalog row back into a function
/// schema. The sidecar stores the catalog as serialized [`ToolSchema`]; a
/// consumer reading it back has JSON, not the struct.
///
/// Returns `None` when the row is not a recognizable canonical schema, so a
/// caller fails closed rather than emitting a half-converted catalog.
pub fn function_schema_from_catalog_row(row: &Value) -> Option<Value> {
    let tool: ToolSchema = serde_json::from_value(row.clone()).ok()?;
    Some(tool_schema_to_function_schema(&tool))
}

fn params_to_json_schema(params: &[ToolParamSchema]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for param in params {
        let mut schema = type_expr_to_json_schema(&param.ty);
        annotate(
            &mut schema,
            &param.description,
            param.default.as_ref(),
            &param.examples,
        );
        properties.insert(param.name.clone(), schema);
        if param.required {
            required.push(Value::String(param.name.clone()));
        }
    }
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("object".to_string()));
    object.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        object.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(object)
}

fn annotate(schema: &mut Value, description: &str, default: Option<&Value>, examples: &[Value]) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if !description.is_empty() {
        object.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    if let Some(default) = default {
        object.insert("default".to_string(), default.clone());
    }
    if !examples.is_empty() {
        object.insert("examples".to_string(), Value::Array(examples.to_vec()));
    }
}

/// The inverse of `json_schema_to_type_expr`, as far as the type algebra
/// allows: a `TypeExpr` records the shape but not the original keyword
/// spelling, so `oneOf` and `anyOf` both come back as `anyOf`, and an
/// all-literal union comes back as `enum`.
pub(crate) fn type_expr_to_json_schema(ty: &TypeExpr) -> Value {
    match ty {
        TypeExpr::Primitive(name) => primitive_schema(name),
        TypeExpr::Literal(value) => serde_json::json!({"const": value}),
        TypeExpr::Array(item) => serde_json::json!({
            "type": "array",
            "items": type_expr_to_json_schema(item),
        }),
        TypeExpr::Union(members) => union_schema(members),
        TypeExpr::Intersection(members) => serde_json::json!({
            "allOf": members.iter().map(type_expr_to_json_schema).collect::<Vec<_>>(),
        }),
        TypeExpr::Object(fields) => object_schema(fields),
        // The catalog references a named type declared elsewhere in the tool
        // document. Reproduce the pointer rather than inventing an inline
        // shape for it.
        TypeExpr::Ref(name) => serde_json::json!({
            "$ref": format!("#/components/schemas/{name}"),
        }),
        // A shape Harn could not map. An empty schema says "any value", which
        // is the honest reading; asserting a concrete type would be a guess.
        TypeExpr::Unknown => Value::Object(Map::new()),
    }
}

fn primitive_schema(name: &str) -> Value {
    match name {
        "any" | "unknown" | "void" => Value::Object(Map::new()),
        "number" | "integer" | "string" | "boolean" | "null" | "object" | "array" => {
            serde_json::json!({"type": name})
        }
        // TypeScript-flavoured spellings the prompt renderer also accepts.
        "bool" => serde_json::json!({"type": "boolean"}),
        "int" => serde_json::json!({"type": "integer"}),
        "float" => serde_json::json!({"type": "number"}),
        other => serde_json::json!({"type": other}),
    }
}

fn union_schema(members: &[TypeExpr]) -> Value {
    if members
        .iter()
        .all(|member| matches!(member, TypeExpr::Literal(_)))
    {
        let values: Vec<Value> = members
            .iter()
            .filter_map(|member| match member {
                TypeExpr::Literal(value) => Some(value.clone()),
                _ => None,
            })
            .collect();
        return serde_json::json!({"enum": values});
    }
    serde_json::json!({
        "anyOf": members.iter().map(type_expr_to_json_schema).collect::<Vec<_>>(),
    })
}

fn object_schema(fields: &[ObjectField]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in fields {
        let mut schema = type_expr_to_json_schema(&field.ty);
        annotate(
            &mut schema,
            field.description.as_deref().unwrap_or_default(),
            field.default.as_ref(),
            &field.examples,
        );
        properties.insert(field.name.clone(), schema);
        if field.required {
            required.push(Value::String(field.name.clone()));
        }
    }
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("object".to_string()));
    object.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        object.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(schema: serde_json::Value) -> serde_json::Value {
        let mut registry = super::super::components::ComponentRegistry::default();
        let ty =
            super::super::json_schema::json_schema_to_type_expr(&schema, &schema, &mut registry);
        type_expr_to_json_schema(&ty)
    }

    #[test]
    fn preserves_optional_properties_the_caller_never_used() {
        // The regression this exists to prevent: a schema inferred from
        // observed arguments would drop `end_line` entirely.
        let declared = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "end_line": {"type": "integer"},
            },
            "required": ["path"],
        });
        let rendered = round_trip(declared);
        assert!(rendered["properties"].get("end_line").is_some());
        assert_eq!(rendered["required"], serde_json::json!(["path"]));
    }

    #[test]
    fn renders_a_literal_union_as_an_enum() {
        let rendered = round_trip(serde_json::json!({"enum": ["a", "b"]}));
        assert_eq!(rendered, serde_json::json!({"enum": ["a", "b"]}));
    }

    #[test]
    fn renders_arrays_with_their_item_type() {
        let rendered = round_trip(serde_json::json!({
            "type": "array",
            "items": {"type": "string"},
        }));
        assert_eq!(
            rendered,
            serde_json::json!({"type": "array", "items": {"type": "string"}})
        );
    }

    #[test]
    fn catalog_row_round_trips_through_its_serialized_form() {
        let tool = ToolSchema {
            name: "read_file".to_string(),
            description: "Read a file.".to_string(),
            params: vec![ToolParamSchema {
                name: "path".to_string(),
                ty: TypeExpr::Primitive("string".to_string()),
                description: "File to read.".to_string(),
                required: true,
                default: None,
                examples: Vec::new(),
            }],
            compact: false,
        };
        let row = serde_json::to_value(&tool).unwrap();
        let rendered = function_schema_from_catalog_row(&row).expect("catalog row converts");
        assert_eq!(rendered["function"]["name"], serde_json::json!("read_file"));
        assert_eq!(
            rendered["function"]["parameters"]["properties"]["path"]["type"],
            serde_json::json!("string")
        );
        assert_eq!(
            rendered["function"]["parameters"]["required"],
            serde_json::json!(["path"])
        );
    }

    #[test]
    fn rejects_a_row_that_is_not_a_canonical_schema() {
        assert!(function_schema_from_catalog_row(&serde_json::json!({"nope": 1})).is_none());
    }
}
