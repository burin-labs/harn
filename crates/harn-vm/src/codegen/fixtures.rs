//! Shared backend conformance inputs lowered by the production catalog owner.

use serde_json::{json, Map, Value};

use super::{compile_catalog, CodegenDiagnostic, CodegenDiagnosticCode, ValidatedCodegenIr};
use crate::tool_registry::{PreparedToolCatalog, ToolCatalog};

pub const CODEGEN_NAMES_FIXTURE_ARTIFACT: &str = "fixtures/harn-codegen-names.ir.json";

/// Reserved words, normalized collisions, and runtime type names shared by
/// target backends. These are portable catalog names, not Harn source syntax.
pub fn codegen_names_fixture() -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    let prepared = PreparedToolCatalog::prepare(names_catalog())
        .map_err(|_| CodegenDiagnostic::new(CodegenDiagnosticCode::InvalidIr))?;
    compile_catalog(&prepared, &[])
}

fn names_catalog() -> ToolCatalog {
    let names = [
        "class",
        "model_dump",
        "with-dash",
        "with_dash",
        "1leading",
        "Δοκιμή",
        "constructor",
        "transport",
        "Uint8Array",
        "ReadonlySet",
        "ReadonlyArray",
        "Record",
        "Readonly",
        "HarnDecimal",
        "from",
        "async",
        "toString",
        "__proto__",
    ];
    let tools = names.iter().enumerate().map(|(index, name)| {
        let mut fields = Map::from_iter(names.iter().map(|field| (field.to_string(), fixture_integer())));
        let mut bytes = Value::Object(crate::value::json::bytes_json_schema(false));
        bytes["x-harn-wire-id"] = json!("codegen-names:bytes");
        fields.extend([
            ("wire_bytes".into(), bytes.clone()),
            ("wire_list".into(), json!({"type":"array","items":fixture_integer(),"x-harn-wire-id":"codegen-names:list"})),
            ("wire_set".into(), json!({"type":"array","items":fixture_integer(),"uniqueItems":true,"x-harn-type":"set","x-harn-wire-id":"codegen-names:set"})),
            ("wire_map".into(), json!({"type":"object","additionalProperties":fixture_integer(),"x-harn-wire-id":"codegen-names:map"})),
            ("wire_decimal".into(), json!({"type":"string","format":"harn-decimal","x-harn-type":"decimal","x-harn-wire-id":"codegen-names:decimal"})),
            ("wire_tuple".into(), json!({"type":"array","prefixItems":[bytes,fixture_integer()],"items":false,"minItems":2,"maxItems":2,"x-harn-wire-id":"codegen-names:tuple"})),
        ]);
        let required = fields.keys().cloned().collect::<Vec<_>>();
        // The CLI owns explicit spellings for property names whose default
        // kebab-case forms collide. Backends still receive the original names.
        let arguments = Map::from_iter(fields.keys().enumerate().map(|(field_index, field)|
            (field.to_string(), json!({"long":format!("field-{field_index}")}))));
        let shape = json!({
            "type":"object", "properties":fields, "required":required,
            "additionalProperties":false,
            "x-harn-wire-id":format!("codegen-names:{index}"),
            "x-harn-nominal":{"declaration_id":format!("codegen-names:{index}"),"name":name,"arguments":[]}
        });
        json!({
            "name":name, "inputSchema":shape, "outputSchema":shape,
            "governance":{"audiences":["cli","mcp","catalog","agent"]},
            "cli":{"command":[format!("op-{index}")],"aliases":[],"hidden":false,"arguments":arguments},
            "deferLoading":false
        })
    }).collect::<Vec<Value>>();
    serde_json::from_value(json!({
        "schema_version":"harn-tools/2.0", "info":{"name":"codegen-name-conformance"}, "tools":tools
    }))
    .expect("owned name fixture is a closed catalog")
}

fn fixture_integer() -> Value {
    json!({"type":"integer","x-harn-wire-id":"codegen-names:integer"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_name_corpus_is_an_actual_validated_ir_with_distinct_nominal_identities() {
        PreparedToolCatalog::prepare(names_catalog())
            .expect("name corpus must pass the ordinary catalog and CLI owner");
        let fixture = codegen_names_fixture().unwrap();
        assert_eq!(fixture.ir().operations.len(), 18);
        let bytes = serde_json::to_vec(fixture.ir()).unwrap();
        let validated = crate::codegen::validate_ir(&bytes).unwrap();
        assert_eq!(validated.ir(), fixture.ir());
        let names = fixture
            .ir()
            .operations
            .iter()
            .map(|operation| operation.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            names.contains("class")
                && names.contains("with-dash")
                && names.contains("with_dash")
                && names.contains("Δοκιμή")
        );
        assert_eq!(
            fixture
                .ir()
                .types
                .iter()
                .filter(|node| matches!(
                    node.shape,
                    crate::wire::WireShape::Record {
                        nominal: Some(_),
                        ..
                    }
                ))
                .count(),
            18
        );
        for kind in ["bytes", "list", "set", "map", "decimal", "tuple"] {
            assert!(
                fixture
                    .ir()
                    .types
                    .iter()
                    .any(|node| { serde_json::to_value(&node.shape).unwrap()["kind"] == kind }),
                "native shape {kind} must coexist with the colliding nominal names"
            );
        }
    }
}
