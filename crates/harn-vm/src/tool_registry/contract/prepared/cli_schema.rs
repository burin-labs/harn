//! CLI token shapes resolved by the same resources as runtime validation.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{PreparedToolCatalogError, RuntimeSchemaResources, RUNTIME_SCHEMA_ROOT_URI};
use crate::tool_registry::ToolCatalogEntry;

pub(super) fn argument_schemas(
    tool: &ToolCatalogEntry,
    resources: &RuntimeSchemaResources,
) -> Result<BTreeMap<String, Value>, PreparedToolCatalogError> {
    let schema = resources.rewrite(&tool.input_schema)?;
    let registry = resources
        .registry
        .add(RUNTIME_SCHEMA_ROOT_URI, schema)
        .and_then(|builder| builder.prepare())
        .map_err(|error| PreparedToolCatalogError::new(error.to_string()))?;
    let root = registry.resolver(
        jsonschema::uri::from_str(RUNTIME_SCHEMA_ROOT_URI).expect("fixed schema root URI"),
    );
    let mut schemas = BTreeMap::new();
    for property in tool.input_schema["properties"]
        .as_object()
        .into_iter()
        .flat_map(|properties| properties.keys())
    {
        let failure = |detail: String| {
            PreparedToolCatalogError::new(format!(
                "tool {:?} property {property:?} {detail}",
                tool.name
            ))
        };
        let path = format!(
            "#/properties/{}",
            super::super::encode_json_pointer_segment(property)
        );
        let located = root
            .lookup(&path)
            .map_err(|error| failure(error.to_string()))?;
        let (schema, mut resolver, _) = located.into_inner();
        let mut resolved = schema.clone();
        let mut seen = vec![schema];
        while let Some(reference) = resolved.get("$ref").and_then(Value::as_str) {
            let located = resolver
                .lookup(reference)
                .map_err(|error| failure(error.to_string()))?;
            let (target, scope, _) = located.into_inner();
            if seen.iter().any(|previous| std::ptr::eq(*previous, target)) {
                return Err(failure("resolves through a cyclic schema reference".into()));
            }
            seen.push(target);
            resolver = scope;
            let mut merged = target.clone();
            if let (Some(object), Some(siblings)) = (merged.as_object_mut(), resolved.as_object()) {
                for (key, value) in siblings {
                    if key != "$ref" {
                        object.insert(key.clone(), value.clone());
                    }
                }
            }
            resolved = merged;
        }
        schemas.insert(property.clone(), resolved);
    }
    Ok(schemas)
}
