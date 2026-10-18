//! Shared schema fixture for projection and native wire serialization checks.

use serde_json::{Map, Value};

pub fn closed_discriminated_union_schema(combinator: &str, literal_keyword: &str) -> Value {
    let literal = |value: &str| {
        let mut schema = Map::new();
        let value = Value::String(value.to_string());
        schema.insert(
            literal_keyword.to_string(),
            if literal_keyword == "enum" {
                Value::Array(vec![value])
            } else {
                value
            },
        );
        Value::Object(schema)
    };
    let mut source = Map::new();
    source.insert(
        combinator.to_string(),
        serde_json::json!([
            {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "kind": literal("cron"), "cron": {"type": "string"},
                    "timezone": {"type": "string"}
                },
                "required": ["kind", "cron", "timezone"]
            },
            {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "kind": literal("external"), "provider": {"type": "string"},
                    "event": {"type": "string"}
                },
                "required": ["kind", "provider", "event"]
            }
        ]),
    );
    serde_json::json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "schema_version": literal("1"), "name": {"type": "string"},
            "description": {"type": "string"}, "goal": {"type": "string"},
            "template": {"enum": [
                "deterministic-sweeper", "hybrid-classify-then-act", "frontier-judgment-loop"
            ]},
            "source": Value::Object(source)
        },
        "required": ["schema_version", "name", "description", "goal", "template", "source"]
    })
}
