use std::collections::BTreeSet;

use super::components::{ref_name_from_pointer, resolve_json_ref, ComponentRegistry};
use super::params::extract_examples;
use super::type_expr::{collapse_members, merge_nullable, ObjectField, TypeExpr};
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
        return collapse_members(members, TypeExpr::Union);
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
            return match collapse_members(members, TypeExpr::Union) {
                TypeExpr::Union(members) => merge_nullable(TypeExpr::Union(members)),
                other => other,
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
        return collapse_members(members, TypeExpr::Intersection);
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
            collapse_members(primitives, TypeExpr::Union)
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

/// True when a `parameters` dict is unambiguously a JSON Schema object rather
/// than Harn's flat `{param_name: type}` map.
///
/// Harn's tool `parameters` is a param map — `{city: "string"}`. Callers coming
/// from the OpenAI or Anthropic tool shapes reach for a full JSON Schema
/// instead, and the flat reading then treats `type`, `properties`, and
/// `required` as three parameter *names*. The provider accepts the resulting
/// nonsense schema and the model fills the schema's own keys in as arguments,
/// so the failure surfaces as a confidently wrong tool call rather than an
/// error. Detect the schema shape and pass it through verbatim.
fn looks_like_json_schema_object(params: &crate::value::DictMap) -> bool {
    let declares_object = params
        .get("type")
        .is_some_and(|value| value.display() == "object");
    declares_object && matches!(params.get("properties"), Some(VmValue::Dict(_)))
}

pub(super) fn vm_build_json_schema(params: Option<&crate::value::DictMap>) -> serde_json::Value {
    if let Some(params) = params {
        if looks_like_json_schema_object(params) {
            let mut json = super::super::vm_value_to_json(&VmValue::dict(params.clone()));
            crate::schema::normalize_json_schema_type_names(&mut json);
            return json;
        }
    }

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    if let Some(params) = params {
        for (name, type_val) in params {
            let (mut schema, is_required) = vm_param_to_json_schema(type_val);
            if let Some(obj) = schema.as_object_mut() {
                if matches!(obj.get("required"), Some(serde_json::Value::Bool(_))) {
                    obj.remove("required");
                }
            }
            properties.insert(name.to_string(), schema);
            if is_required {
                required.push(serde_json::Value::String(name.to_string()));
            }
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn vm_param_to_json_schema(value: &VmValue) -> (serde_json::Value, bool) {
    match value {
        VmValue::String(type_name) => (
            serde_json::json!({
                "type": crate::schema::json_schema_type_name(type_name).unwrap_or("string")
            }),
            true,
        ),
        VmValue::Dict(dict) => {
            let mut json = super::super::vm_value_to_json(value);
            crate::schema::normalize_json_schema_type_names(&mut json);
            let is_required = dict
                .get("required")
                .and_then(|value| match value {
                    VmValue::Bool(required) => Some(*required),
                    _ => None,
                })
                .unwrap_or(true)
                && !dict.contains_key("default");
            (json, is_required)
        }
        _ => (serde_json::json!({"type": "string"}), true),
    }
}

#[cfg(test)]
mod json_schema_shape_tests {
    use super::vm_build_json_schema;
    use crate::value::{DictMap, VmDictExt, VmValue};

    fn dict(pairs: Vec<(&str, VmValue)>) -> DictMap {
        let mut map = DictMap::new();
        for (key, value) in pairs {
            map.insert(crate::value::intern_key(key), value);
        }
        map
    }

    #[test]
    fn flat_param_map_builds_an_object_schema() {
        let params = dict(vec![("city", VmValue::string("string"))]);
        let schema = vm_build_json_schema(Some(&params));
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["city"]["type"], "string");
        assert_eq!(schema["required"], serde_json::json!(["city"]));
    }

    #[test]
    fn a_full_json_schema_passes_through_instead_of_being_read_as_param_names() {
        // Reproduces a live misfire on claude-opus-5: a caller passing the
        // OpenAI/Anthropic tool shape got `type`, `properties`, and `required`
        // treated as parameter names, and the model answered with
        // `{"properties": {"city": "La Paz"}, "required": "city", ...}`.
        let mut city = DictMap::new();
        city.put_str("type", "string");
        let properties = dict(vec![("city", VmValue::dict(city))]);
        let params = dict(vec![
            ("type", VmValue::string("object")),
            ("properties", VmValue::dict(properties)),
            (
                "required",
                VmValue::List(std::sync::Arc::new(vec![VmValue::string("city")])),
            ),
        ]);

        let schema = vm_build_json_schema(Some(&params));
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["city"]["type"], "string");
        assert_eq!(schema["required"], serde_json::json!(["city"]));
        assert!(
            schema["properties"].get("properties").is_none(),
            "the schema's own keys must not become parameters: {schema}"
        );
    }

    #[test]
    fn a_param_literally_named_type_still_reads_as_a_param_map() {
        // Only the full `{type: "object", properties: {...}}` pair triggers
        // pass-through, so a tool that genuinely takes a `type` argument is
        // unaffected.
        let params = dict(vec![
            ("type", VmValue::string("string")),
            ("value", VmValue::string("int")),
        ]);
        let schema = vm_build_json_schema(Some(&params));
        assert_eq!(schema["properties"]["type"]["type"], "string");
        assert_eq!(schema["properties"]["value"]["type"], "integer");
    }
}
