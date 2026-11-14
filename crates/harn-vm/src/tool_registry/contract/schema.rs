//! Draft 2020-12 schema traversal and JSON Pointer primitives.
//!
//! Callers operate only on schema-bearing keyword positions so reference-shaped
//! values under `const` and `enum` remain opaque instance data.

use serde_json::Value as JsonValue;

const SINGLE_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
const ARRAY_SUBSCHEMA_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
const MAP_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
];

pub(super) fn canonicalize_required_names(schema: &mut JsonValue) {
    try_transform_schema_nodes(schema, &mut |object| {
        if let Some(names) = object.get_mut("required").and_then(JsonValue::as_array_mut) {
            if names.iter().all(JsonValue::is_string) {
                names.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            }
        }
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {});
}

pub(super) fn try_visit_schema_nodes<E>(
    schema: &JsonValue,
    visitor: &mut impl FnMut(&serde_json::Map<String, JsonValue>) -> Result<(), E>,
) -> Result<(), E> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    visitor(object)?;
    for keyword in SINGLE_SUBSCHEMA_KEYWORDS {
        if let Some(child) = object.get(*keyword) {
            try_visit_schema_nodes(child, visitor)?;
        }
    }
    for keyword in ARRAY_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get(*keyword).and_then(JsonValue::as_array) {
            for child in children {
                try_visit_schema_nodes(child, visitor)?;
            }
        }
    }
    for keyword in MAP_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get(*keyword).and_then(JsonValue::as_object) {
            for child in children.values() {
                try_visit_schema_nodes(child, visitor)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn try_transform_schema_nodes<E>(
    schema: &mut JsonValue,
    visitor: &mut impl FnMut(&mut serde_json::Map<String, JsonValue>) -> Result<(), E>,
) -> Result<(), E> {
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };
    visitor(object)?;
    for keyword in SINGLE_SUBSCHEMA_KEYWORDS {
        if let Some(child) = object.get_mut(*keyword) {
            try_transform_schema_nodes(child, visitor)?;
        }
    }
    for keyword in ARRAY_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get_mut(*keyword).and_then(JsonValue::as_array_mut) {
            for child in children {
                try_transform_schema_nodes(child, visitor)?;
            }
        }
    }
    for keyword in MAP_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get_mut(*keyword).and_then(JsonValue::as_object_mut) {
            for child in children.values_mut() {
                try_transform_schema_nodes(child, visitor)?;
            }
        }
    }
    Ok(())
}

pub(super) fn decode_json_pointer_segment(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        decoded.push(match chars.next()? {
            '0' => '~',
            '1' => '/',
            _ => return None,
        });
    }
    Some(decoded)
}

pub(super) fn encode_json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_name_canonicalization_preserves_duplicates_and_opaque_instance_arrays() {
        let instance = json!({"required":["z","a"]});
        let mut schema = json!({"required":["z","a","a"], "properties":{
            "payload":{"const":instance,"default":instance,"enum":[instance]},
            "nested":{"type":"object","required":["z","a"]}
        }});
        canonicalize_required_names(&mut schema);
        assert_eq!(schema["required"], json!(["a", "a", "z"]));
        assert_eq!(
            schema["properties"]["nested"]["required"],
            json!(["a", "z"])
        );
        for keyword in ["const", "default"] {
            assert_eq!(schema["properties"]["payload"][keyword], instance);
        }
        assert_eq!(schema["properties"]["payload"]["enum"], json!([instance]));
    }
}
