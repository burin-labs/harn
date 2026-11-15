use std::collections::BTreeMap;

use serde_json::Value;

/// Host boundary canonicalization resolves local references at the boundary
/// root. Retain the resource and project its named definitions for that owner.
pub(crate) fn embed_shared_resource(
    boundary: &mut Value,
    property: &str,
    name: &str,
    resource: &Value,
) -> Result<bool, String> {
    let identifier = resource
        .get("$id")
        .and_then(Value::as_str)
        .ok_or("shared schema resource has no identifier")?;
    if boundary
        .get("properties")
        .and_then(|fields| fields.get(property))
        .and_then(|field| field.get("$ref"))
        .and_then(Value::as_str)
        != Some(identifier)
    {
        return Ok(false);
    }
    let mut projected = BTreeMap::from([(name.to_owned(), resource.clone())]);
    if let Some(definitions) = resource.get("$defs") {
        for (key, definition) in definitions
            .as_object()
            .ok_or("shared schema definitions must be an object")?
        {
            if let Some(previous) = projected.insert(key.clone(), definition.clone()) {
                if previous != *definition {
                    return Err(format!("conflicting shared schema definition {key:?}"));
                }
            }
        }
    }
    if let Some(definitions) = boundary.get("$defs") {
        let definitions = definitions
            .as_object()
            .ok_or("boundary schema definitions must be an object")?;
        for (key, definition) in &projected {
            if definitions
                .get(key)
                .is_some_and(|previous| previous != definition)
            {
                return Err(format!("conflicting shared schema definition {key:?}"));
            }
        }
    }
    for (key, definition) in projected {
        boundary["$defs"][key] = definition;
    }
    let pointer = name.replace('~', "~0").replace('/', "~1");
    boundary["properties"][property]["$ref"] = Value::String(format!("#/$defs/{pointer}"));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shared_local_definitions_reach_the_boundary_without_changing_the_resource() {
        let resource = json!({"$id": "urn:shared-control", "$defs": {"clause": {"type": "integer"}},
            "type": "object", "properties": {"clause": {"$ref": "#/$defs/clause"}}});
        let mut boundary = json!({"properties": {"stage": {"$ref": "urn:shared-control"}}});
        assert!(embed_shared_resource(&mut boundary, "stage", "shared", &resource).unwrap());
        assert_eq!(boundary["properties"]["stage"]["$ref"], "#/$defs/shared");
        assert_eq!(boundary["$defs"]["shared"], resource);
        assert_eq!(boundary["$defs"]["clause"], json!({"type": "integer"}));
        let before = boundary.clone();
        assert!(!embed_shared_resource(&mut boundary, "other", "shared", &resource).unwrap());
        assert_eq!(boundary, before);
    }

    #[test]
    fn conflicting_definition_refuses_without_overwriting_the_boundary() {
        let resource =
            json!({"$id": "urn:shared-control", "$defs": {"clause": {"type": "integer"}}});
        let mut boundary = json!({"properties": {"stage": {"$ref": "urn:shared-control"}},
            "$defs": {"clause": {"type": "string"}}});
        let before = boundary.clone();
        assert!(embed_shared_resource(&mut boundary, "stage", "shared", &resource).is_err());
        assert_eq!(boundary, before);
    }
}
