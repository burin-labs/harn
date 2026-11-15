use serde_json::{json, Value};

use crate::tool_registry::{PreparedToolCatalog, ToolCatalog, ToolContractPhase};
use crate::wire::{PreparedWireGraph, WireFailureCode, WireShape};
use crate::VmValue;

fn catalog(metadata: Value) -> ToolCatalog {
    let schema = json!({
        "type":"object", "properties":{
            "blob":Value::Object(crate::value::json::bytes_json_schema(false)),
            "metadata": metadata
        }, "required":["blob","metadata"], "additionalProperties":false
    });
    serde_json::from_value(json!({
        "schema_version":"harn-tools/2.0", "info":{"name":"mixed-json"},
        "tools":[{"name":"echo", "inputSchema":schema, "outputSchema":schema,
            "governance":{"audiences":["mcp","cli"]},
            "cli":{"command":["echo"],"aliases":[],"hidden":false,"arguments":{}},
            "deferLoading":false}]
    }))
    .unwrap()
}

fn payload() -> Value {
    json!({"blob":{"$bytes_b64":"YWJj"},"metadata":{
        "label":"known-positive", "$bytes_b64":"ordinary-data",
        "nested":{"enum":"ordinary","variant":"data","fields":[null,true,7,1.25]},
        "x-harn-type":"opaque application data"
    }})
}

#[test]
fn mixed_catalog_hydrates_declared_bytes_and_preserves_open_json_values() {
    let prepared = PreparedToolCatalog::prepare(catalog(
        json!({"type":"object","additionalProperties":true}),
    ))
    .expect("runtime prepares mixed typed and gradual schema");
    let value = prepared.decode_input("echo", &payload()).unwrap();
    let fields = value.as_dict().unwrap();
    assert!(
        matches!(fields.get("blob"), Some(VmValue::Bytes(bytes)) if bytes.as_slice() == b"abc")
    );
    let metadata = fields.get("metadata").unwrap();
    assert_eq!(
        crate::value::json::vm_value_to_json(metadata),
        payload()["metadata"]
    );
    let encoded = prepared
        .encode_wire_value("echo", ToolContractPhase::Output, &value)
        .unwrap();
    prepared.validate_output("echo", &encoded).unwrap();
    assert_eq!(encoded, payload());
    let wire = prepared
        .prepared_wire_type("echo", ToolContractPhase::Input)
        .unwrap()
        .unwrap();
    assert!(wire
        .graph()
        .types()
        .any(|ty| matches!(ty.shape, WireShape::RuntimeJson)));
    assert!(jsonschema::is_valid(&wire.json_schema(), &payload()));
}

#[test]
fn runtime_islands_do_not_weaken_the_original_schema_or_typed_output() {
    let prepared = PreparedToolCatalog::prepare(catalog(json!({
        "type":"object", "properties":{"label":{"type":"string","minLength":3}},
        "required":["label"], "additionalProperties":true
    })))
    .unwrap();
    let value = prepared.decode_input("echo", &payload()).unwrap();
    let mut wrong = payload();
    wrong["metadata"]["label"] = json!("x");
    assert!(prepared.decode_input("echo", &wrong).is_err());
    wrong = payload();
    wrong["blob"] = json!({"$bytes_b64":"YR=="});
    assert!(prepared.decode_input("echo", &wrong).is_err());
    let wrong_runtime = crate::value::json::json_to_vm_value(&payload());
    let error = prepared
        .encode_wire_value("echo", ToolContractPhase::Output, &wrong_runtime)
        .unwrap_err();
    assert_eq!(
        error.wire_failure.unwrap().code,
        WireFailureCode::WrongRuntimeType
    );
    assert_eq!(
        prepared
            .encode_wire_value("echo", ToolContractPhase::Output, &value)
            .unwrap(),
        payload()
    );
}

#[test]
fn unsupported_schema_cannot_hide_declared_wire_types_behind_references() {
    let mut raw = catalog(json!({"type":"object","additionalProperties":true}));
    let hidden = json!({"allOf":[{"$ref":"#/components/schemas/Hidden"}]});
    raw.tools[0].input_schema["properties"]["metadata"] = hidden.clone();
    raw.tools[0].output_schema.as_mut().unwrap()["properties"]["metadata"] = hidden;
    raw.components = Some(crate::tool_registry::ToolCatalogComponents {
        schemas: std::collections::BTreeMap::from([(
            "Hidden".into(),
            Value::Object(crate::value::json::bytes_json_schema(false)),
        )]),
    });
    assert!(PreparedToolCatalog::prepare(raw).is_err());
    PreparedToolCatalog::prepare(catalog(
        json!({"type":"object","additionalProperties":true}),
    ))
    .expect("the unannotated JSON island remains supported");
}

#[test]
fn annotation_shaped_instance_data_is_not_schema_authority() {
    let instance = json!({"x-harn-type":"bytes","$ref":"not-a-schema-resource"});
    let prepared = PreparedToolCatalog::prepare(catalog(json!({
        "const":instance, "default":instance, "examples":[instance]
    })))
    .unwrap();
    let mut input = payload();
    input["metadata"] = instance;
    let value = prepared.decode_input("echo", &input).unwrap();
    assert_eq!(
        prepared
            .encode_wire_value("echo", ToolContractPhase::Output, &value)
            .unwrap(),
        input
    );
}

#[test]
fn runtime_json_cannot_enter_portable_ir_or_its_projections() {
    let prepared = PreparedToolCatalog::prepare(catalog(
        json!({"type":"object","additionalProperties":true}),
    ))
    .unwrap();
    let wire = prepared
        .prepared_wire_type("echo", ToolContractPhase::Input)
        .unwrap()
        .unwrap();
    let runtime = wire
        .graph()
        .types()
        .find(|ty| matches!(ty.shape, WireShape::RuntimeJson))
        .unwrap();
    assert!(serde_json::to_value(runtime).is_err());
    assert!(serde_json::from_value::<WireShape>(json!({"kind":"runtime_json"})).is_err());
    assert_eq!(
        PreparedWireGraph::prepare(wire.graph().types().cloned())
            .unwrap_err()
            .code,
        WireFailureCode::UnsupportedType
    );
    assert_eq!(
        PreparedWireGraph::prepare_runtime(wire.graph().types().cloned(), Default::default())
            .unwrap_err()
            .code,
        WireFailureCode::InvalidContract
    );
    assert_eq!(
        crate::codegen::compile_catalog(&prepared, &[])
            .unwrap_err()
            .code,
        crate::codegen::CodegenDiagnosticCode::UnsupportedWireType
    );
    let schema = serde_json::to_value(schemars::schema_for!(WireShape)).unwrap();
    assert!(!schema.to_string().contains("runtime_json"));
    assert!(!crate::codegen::codegen_typescript().contains("runtime_json"));
}

#[test]
fn untyped_gradual_schemas_keep_the_existing_json_boundary() {
    let mut raw = catalog(json!({}));
    let schema =
        json!({"type":"object","patternProperties":{"^meta_":true},"additionalProperties":false});
    raw.tools[0].input_schema = schema.clone();
    raw.tools[0].output_schema = Some(schema);
    let prepared = PreparedToolCatalog::prepare(raw).unwrap();
    assert!(prepared
        .prepared_wire_type("echo", ToolContractPhase::Input)
        .is_err());
    let input = json!({"meta_value":{"$bytes_b64":"YWJj","enum":"ordinary-data"}});
    let value = prepared.decode_input("echo", &input).unwrap();
    assert_eq!(crate::value::json::vm_value_to_json(&value), input);
    assert_eq!(
        prepared
            .encode_wire_value("echo", ToolContractPhase::Output, &value)
            .unwrap(),
        input
    );
    assert!(prepared
        .decode_input("echo", &json!({"not_permitted":7}))
        .is_err());
}

#[test]
fn closed_declared_wire_schemas_still_compile_into_portable_ir() {
    let prepared = PreparedToolCatalog::prepare(catalog(json!({
        "type":"object", "properties":{"label":{"type":"string"}},
        "required":["label"], "additionalProperties":false
    })))
    .unwrap();
    let ir = crate::codegen::compile_catalog(&prepared, &[]).unwrap();
    crate::codegen::validate_ir(&ir.canonical_json()).unwrap();
    let input = json!({"blob":{"$bytes_b64":"YWJj"},"metadata":{"label":"known-positive"}});
    let value = prepared.decode_input("echo", &input).unwrap();
    assert_eq!(
        prepared
            .encode_wire_value("echo", ToolContractPhase::Output, &value)
            .unwrap(),
        input
    );
    assert!(ir
        .ir()
        .types
        .iter()
        .all(|ty| !matches!(ty.shape, WireShape::RuntimeJson)));
}

#[test]
fn runtime_union_members_validate_their_resolved_json_schema() {
    let mut raw = catalog(json!({"type":"object","additionalProperties":true}));
    raw.tools[0].output_schema = Some(json!({"anyOf":[
        Value::Object(crate::value::json::bytes_json_schema(false)),
        {"allOf":[{"$ref":"#/components/schemas/PlainText"}]}
    ]}));
    raw.components = Some(crate::tool_registry::ToolCatalogComponents {
        schemas: std::collections::BTreeMap::from([(
            "PlainText".into(),
            json!({"type":"string","pattern":"^ok-"}),
        )]),
    });
    let prepared = PreparedToolCatalog::prepare(raw).unwrap();
    let wire = prepared
        .prepared_wire_type("echo", ToolContractPhase::Output)
        .unwrap()
        .unwrap();
    let text = wire.decode(&json!("ok-text")).unwrap();
    assert!(matches!(&text, crate::VmValue::String(value) if value.as_str() == "ok-text"));
    assert_eq!(wire.encode(&text).unwrap(), json!("ok-text"));
    let bytes = wire.decode(&json!({"$bytes_b64":"YWJj"})).unwrap();
    assert!(matches!(&bytes, VmValue::Bytes(value) if value.as_slice() == b"abc"));
    assert_eq!(wire.encode(&bytes).unwrap(), json!({"$bytes_b64":"YWJj"}));
    assert!(wire.decode(&json!("wrong-prefix")).is_err());
    assert!(wire.encode(&VmValue::string("wrong-prefix")).is_err());
    assert!(wire.decode(&Value::Null).is_err());
}

#[test]
fn runtime_union_never_guesses_between_typed_and_opaque_interpretations() {
    let mut raw = catalog(json!({"type":"object","additionalProperties":true}));
    raw.tools[0].output_schema = Some(json!({"anyOf":[
        Value::Object(crate::value::json::bytes_json_schema(false)), true
    ]}));
    let prepared = PreparedToolCatalog::prepare(raw.clone()).unwrap();
    let wire = prepared
        .prepared_wire_type("echo", ToolContractPhase::Output)
        .unwrap()
        .unwrap();
    assert!(
        matches!(wire.decode(&json!("known opaque positive")).unwrap(),
        crate::VmValue::String(value) if value.as_str() == "known opaque positive")
    );
    assert!(wire.decode(&json!({"$bytes_b64":"YWJj"})).is_err());
    assert!(wire
        .encode(&VmValue::Bytes(std::sync::Arc::new(b"abc".to_vec())))
        .is_err());
    raw.tools[0].output_schema.as_mut().unwrap()["anyOf"]
        .as_array_mut()
        .unwrap()
        .extend([json!({"type":"integer"}), json!({"type":"number"})]);
    assert!(
        PreparedToolCatalog::prepare(raw).is_err(),
        "closed numeric ambiguity remains a preparation refusal even beside a JSON island"
    );
}

fn observed_openapi_catalog(fixture: &str) -> PreparedToolCatalog {
    let observed: Value = serde_json::from_str(fixture).unwrap();
    let mut raw = catalog(json!({}));
    let template = raw.tools[0].clone();
    raw.tools = observed["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|adapter| {
            let mut tool = template.clone();
            tool.name = adapter["name"].as_str().unwrap().to_owned();
            tool.cli.command = vec![tool.name.clone()];
            tool.input_schema = adapter["input_schema"].clone();
            tool.output_schema = Some(adapter["output_schema"].clone());
            tool
        })
        .collect();
    raw.components = Some(serde_json::from_value(observed["components"].clone()).unwrap());
    PreparedToolCatalog::prepare(raw).unwrap()
}

#[test]
fn observed_openapi_multipart_contract_hydrates_bytes_without_dropping_part_constraints() {
    let prepared = observed_openapi_catalog(include_str!("fixtures/openapi-media.json"));
    let input = json!({"body":{
        "file":{"value":{"$bytes_b64":"AAEC/w=="},"headers":{"X-Part":"known-part"},"filename":"data.bin","content_type":"application/octet-stream"},
        "title":{"value":"known title","content_type":"text/plain"},
        "tags":[{"value":"first"},{"value":"second"}]
    }});
    let decoded = prepared.decode_input("send_multipart", &input).unwrap();
    let body = decoded
        .as_dict()
        .unwrap()
        .get("body")
        .unwrap()
        .as_dict()
        .unwrap();
    let file = body.get("file").unwrap().as_dict().unwrap();
    assert!(
        matches!(file.get("value"), Some(VmValue::Bytes(bytes)) if bytes.as_slice() == b"\0\x01\x02\xff")
    );
    assert_eq!(
        prepared
            .encode_wire_value("send_multipart", ToolContractPhase::Input, &decoded)
            .unwrap(),
        input
    );
    let mut wrong = input.clone();
    wrong["body"]["file"]["content_type"] = json!("application/json");
    assert!(prepared.decode_input("send_multipart", &wrong).is_err());
    wrong = input;
    wrong["body"]["file"]["headers"] = json!({});
    assert!(prepared.decode_input("send_multipart", &wrong).is_err());
}

#[test]
fn observed_openapi_media_union_keeps_local_definition_references_in_the_original_document() {
    for (fixture, name) in [
        (
            include_str!("fixtures/openapi-media.json"),
            "receive_multiple",
        ),
        (
            include_str!("fixtures/openapi-response.json"),
            "get_multiple",
        ),
    ] {
        let prepared = observed_openapi_catalog(fixture);
        let wire = prepared
            .prepared_wire_type(name, ToolContractPhase::Output)
            .unwrap()
            .unwrap();
        for input in [json!("plain Δ response"), json!({"$bytes_b64":"AAEC/w=="})] {
            let decoded = wire.decode(&input).unwrap();
            if input.is_object() {
                assert!(
                    matches!(&decoded, VmValue::Bytes(bytes) if bytes.as_slice() == b"\0\x01\x02\xff")
                );
            } else {
                assert!(
                    matches!(&decoded, crate::VmValue::String(value) if value.as_str() == "plain Δ response")
                );
            }
            assert_eq!(wire.encode(&decoded).unwrap(), input);
        }
        assert!(wire.decode(&json!(7)).is_err());
        assert!(wire.encode(&VmValue::Int(7)).is_err());
    }
}
