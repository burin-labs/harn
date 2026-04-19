use std::collections::BTreeSet;

use super::components::{ref_name_from_pointer, resolve_json_ref, ComponentRegistry};
use super::params::extract_examples;
use super::type_expr::{merge_nullable, ObjectField, TypeExpr};
use crate::value::VmValue;

/// Convert a JSON Schema fragment into a TypeExpr, recursing through
/// oneOf/anyOf/allOf, items, properties, const/enum, and $ref. The `root`
/// document is required to resolve ref pointers; the `registry` accumulates
/// named types so they can be rendered as top-of-prompt `type X = ...` aliases.
pub(crate) fn json_schema_to_type_expr(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> TypeExpr {
    let obj = match schema.as_object() {
        Some(obj) => obj,
        None => {
            // A bare string in the schema slot means "type name" — be forgiving.
            if let Some(s) = schema.as_str() {
                return TypeExpr::Primitive(s.to_string());
            }
            return TypeExpr::Unknown;
        }
    };

    // $ref — resolve against root, register the resolved type under its short
    // name, and return a Ref so the rendered prompt can share the alias.
    if let Some(serde_json::Value::String(pointer)) = obj.get("$ref") {
        if let Some(name) = ref_name_from_pointer(pointer) {
            if !registry.contains(&name) && !registry.is_in_progress(&name) {
                if let Some(resolved) = resolve_json_ref(root, pointer) {
                    registry.begin_resolution(&name);
                    let expanded = json_schema_to_type_expr(resolved, root, registry);
                    registry.finish_resolution(&name);
                    registry.register(name.clone(), expanded);
                }
            }
            return TypeExpr::Ref(name);
        }
        return TypeExpr::Unknown;
    }

    // const — single-literal type.
    if let Some(constant) = obj.get("const") {
        return TypeExpr::Literal(constant.clone());
    }

    // enum — union of literals.
    if let Some(serde_json::Value::Array(values)) = obj.get("enum") {
        let members: Vec<TypeExpr> = values
            .iter()
            .map(|value| TypeExpr::Literal(value.clone()))
            .collect();
        return match members.len() {
            0 => TypeExpr::Unknown,
            1 => members.into_iter().next().unwrap(),
            _ => TypeExpr::Union(members),
        };
    }

    // oneOf / anyOf — union. Render both the same way for our purposes
    // (model doesn't care about structural-disambiguation semantics here).
    for key in ["oneOf", "anyOf"] {
        if let Some(serde_json::Value::Array(variants)) = obj.get(key) {
            let members: Vec<TypeExpr> = variants
                .iter()
                .map(|value| json_schema_to_type_expr(value, root, registry))
                .filter(|ty| !matches!(ty, TypeExpr::Unknown))
                .collect();
            return match members.len() {
                0 => TypeExpr::Unknown,
                1 => members.into_iter().next().unwrap(),
                _ => merge_nullable(TypeExpr::Union(members)),
            };
        }
    }

    // allOf — intersection of all component schemas.
    if let Some(serde_json::Value::Array(variants)) = obj.get("allOf") {
        let members: Vec<TypeExpr> = variants
            .iter()
            .map(|value| json_schema_to_type_expr(value, root, registry))
            .filter(|ty| !matches!(ty, TypeExpr::Unknown))
            .collect();
        return match members.len() {
            0 => TypeExpr::Unknown,
            1 => members.into_iter().next().unwrap(),
            _ => TypeExpr::Intersection(members),
        };
    }

    // type — may be a string (`"string"`) or an array of strings (`["string", "null"]`).
    let nullable = obj
        .get("nullable")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let core_type = match obj.get("type") {
        Some(serde_json::Value::Array(type_list)) => {
            let primitives: Vec<TypeExpr> = type_list
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .map(|kind| TypeExpr::Primitive(kind.to_string()))
                })
                .collect();
            match primitives.len() {
                0 => TypeExpr::Unknown,
                1 => primitives.into_iter().next().unwrap(),
                _ => TypeExpr::Union(primitives),
            }
        }
        Some(serde_json::Value::String(kind)) => match kind.as_str() {
            "array" => {
                let item_schema = obj.get("items").cloned().unwrap_or(serde_json::json!({}));
                let item_type = json_schema_to_type_expr(&item_schema, root, registry);
                TypeExpr::Array(Box::new(item_type))
            }
            "object" => {
                if let Some(props) = obj.get("properties").and_then(|value| value.as_object()) {
                    let required_set: BTreeSet<String> = obj
                        .get("required")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut fields: Vec<ObjectField> = props
                        .iter()
                        .map(|(name, sub_schema)| ObjectField {
                            name: name.clone(),
                            ty: json_schema_to_type_expr(sub_schema, root, registry),
                            required: required_set.contains(name),
                            description: sub_schema
                                .get("description")
                                .and_then(|value| value.as_str())
                                .map(str::to_string),
                            default: sub_schema.get("default").cloned(),
                            examples: sub_schema
                                .as_object()
                                .map(extract_examples)
                                .unwrap_or_default(),
                        })
                        .collect();
                    // Required first, then optional, stable within each group.
                    fields.sort_by_key(|field| !field.required);
                    TypeExpr::Object(fields)
                } else {
                    TypeExpr::Primitive("object".to_string())
                }
            }
            other => TypeExpr::Primitive(other.to_string()),
        },
        _ => TypeExpr::Unknown,
    };

    if nullable {
        merge_nullable(TypeExpr::Union(vec![
            core_type,
            TypeExpr::Primitive("null".to_string()),
        ]))
    } else {
        core_type
    }
}

pub(super) fn vm_build_json_schema(
    params: Option<&std::collections::BTreeMap<String, VmValue>>,
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    if let Some(params) = params {
        for (name, type_val) in params {
            let type_str = type_val.display();
            let json_type = match type_str.as_str() {
                "int" | "integer" => "integer",
                "float" | "number" => "number",
                "bool" | "boolean" => "boolean",
                "list" | "array" => "array",
                "dict" | "object" => "object",
                _ => "string",
            };
            properties.insert(name.clone(), serde_json::json!({"type": json_type}));
            required.push(serde_json::Value::String(name.clone()));
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}
