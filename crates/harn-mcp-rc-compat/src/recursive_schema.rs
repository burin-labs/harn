//! JSON Schema 2020-12 fixture with recursive `$defs` references.
//!
//! The RC profile pins `https://json-schema.org/draft/2020-12/schema`
//! as the dialect for every tool input schema. Recursive references are
//! the common stress case (e.g. trees, linked lists, ASTs) and the one
//! that trips draft 4 / draft 7 validators. Wiring at least one fixture
//! through `jsonschema::draft202012` is what the acceptance criteria
//! call out.

use serde_json::{json, Value as JsonValue};

/// Build a tool input schema that uses `$defs/Node` with a `children`
/// array of `$ref: "#/$defs/Node"` — the canonical recursive shape.
pub fn recursive_tree_input_schema() -> JsonValue {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://harnlang.com/mcp-rc/recursive-tree.input.schema.json",
        "title": "RecursiveTreeInput",
        "type": "object",
        "properties": {
            "root": { "$ref": "#/$defs/Node" }
        },
        "required": ["root"],
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "minLength": 1 },
                    "children": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }
    })
}

/// A valid recursive instance: a 3-level tree.
pub fn valid_tree_instance() -> JsonValue {
    json!({
        "root": {
            "name": "root",
            "children": [
                { "name": "a", "children": [
                    { "name": "a.1" }
                ]},
                { "name": "b" }
            ]
        }
    })
}

/// An invalid instance: a nested child is missing the required `name`.
pub fn invalid_tree_instance() -> JsonValue {
    json!({
        "root": {
            "name": "root",
            "children": [
                { "children": [] }
            ]
        }
    })
}

/// Validate `instance` against [`recursive_tree_input_schema`] using
/// draft 2020-12. Returns `Ok` on success or an error message that
/// concatenates every validation failure.
pub fn validate(instance: &JsonValue) -> Result<(), String> {
    let schema = recursive_tree_input_schema();
    let validator = jsonschema::draft202012::new(&schema)
        .map_err(|err| format!("compile recursive schema: {err}"))?;
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|err| format!("{err}"))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
