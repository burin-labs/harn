use serde_json::{json, Value};

use super::*;
use crate::tool_registry::{PreparedToolCatalog, ToolCatalog};

pub(super) fn catalog(description: &str) -> PreparedToolCatalog {
    let catalog: ToolCatalog = serde_json::from_value(json!({
        "schema_version":"harn-tools/2.0", "info":{"name":"pathological"},
        "tools":[{"name":"constructor", "description":description,
            "inputSchema":{"type":"object","properties":{
                "description":{"type":"integer","minimum":-9223372036854775808_i64,"maximum":9223372036854775807_i64},
                "optional":{"type":"string"}},"required":["description"],"additionalProperties":false},
            "outputSchema":{"type":"integer"},"governance":{"audiences":["mcp","cli"]},
            "cli":{"command":["constructor"],"aliases":[],"hidden":false,"arguments":{}},
            "deferLoading":false}]
    })).expect("catalog shape");
    PreparedToolCatalog::prepare(catalog).expect("runtime prepares catalog")
}

#[test]
fn generated_typescript_uses_the_shared_projection_whitespace_owner() {
    let generated = codegen_typescript();
    assert!(generated.contains("export type CodegenIr ="));
    assert!(generated.ends_with('\n'));
    assert!(generated.lines().all(|line| line == line.trim_end()));
    assert!(!generated.ends_with("\n\n"));
}

#[test]
fn canonical_catalog_ir_roundtrips_through_the_untrusted_boundary() {
    let ir = compile_catalog(&catalog("test"), &[]).expect("compile");
    let decoded = validate_ir(&ir.canonical_json()).expect("validated standalone IR");
    assert_eq!(decoded.ir(), ir.ir());
    let graph = crate::wire::PreparedWireGraph::prepare(ir.ir().types.clone()).expect("graph");
    let operation = &ir.ir().operations[0];
    let value = json!({"description":9007199254740993_i64});
    let decoded = graph.decode(&operation.input, &value).expect("exact input");
    assert_eq!(graph.encode(&operation.input, &decoded).unwrap(), value);
    assert!(
        serde_json::to_vec(&graph.encode(&operation.input, &decoded).unwrap())
            .unwrap()
            .windows(16)
            .any(|window| window == b"9007199254740993")
    );
}

#[test]
fn unordered_schema_required_names_have_identical_ir_and_binding_identity() {
    let mut raw = catalog("required permutation").catalog().clone();
    raw.tools[0].input_schema["required"] = json!(["description", "optional"]);
    let first = compile_catalog(&PreparedToolCatalog::prepare(raw.clone()).unwrap(), &[]).unwrap();
    raw.tools[0].input_schema["required"] = json!(["optional", "description"]);
    let reordered = compile_catalog(&PreparedToolCatalog::prepare(raw).unwrap(), &[]).unwrap();
    assert_eq!(
        first.canonical_json(),
        reordered.canonical_json(),
        "required names are a schema set"
    );
}

#[test]
fn explicit_selection_order_is_unordered_but_tuple_positions_change_the_wire_abi() {
    let mut raw = catalog("selection permutation").catalog().clone();
    let mut second = raw.tools[0].clone();
    second.name = "other".into();
    second.cli.command = vec!["other".into()];
    raw.tools.push(second);
    let prepared = PreparedToolCatalog::prepare(raw.clone()).unwrap();
    let first = compile_catalog(&prepared, &["constructor".into(), "other".into()]).unwrap();
    let reordered = compile_catalog(&prepared, &["other".into(), "constructor".into()]).unwrap();
    assert_eq!(first.canonical_json(), reordered.canonical_json());
    raw.tools[0].output_schema = Some(
        json!({"type":"array","prefixItems":[{"type":"integer"},{"type":"string"}],"items":false,"minItems":2,"maxItems":2}),
    );
    let first = compile_catalog(&PreparedToolCatalog::prepare(raw.clone()).unwrap(), &[]).unwrap();
    raw.tools[0].output_schema.as_mut().unwrap()["prefixItems"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let reordered = compile_catalog(&PreparedToolCatalog::prepare(raw).unwrap(), &[]).unwrap();
    assert_ne!(
        first.ir().identity.binding_digest,
        reordered.ir().identity.binding_digest
    );
    assert_ne!(
        first.ir().identity.ir_digest,
        reordered.ir().identity.ir_digest
    );
}

#[test]
fn deep_flat_graph_reaches_the_digest_gate_without_recursive_host_stack_growth() {
    use crate::wire::{WireField, WireShape, WireSourceRef, WireType, WireTypeId};

    const DEPTH: usize = 20_000;
    let source = WireSourceRef::Synthetic {
        owner: "flat-graph".into(),
    };
    let node = |id: String, shape| WireType {
        id: WireTypeId(id),
        name: None,
        source: source.clone(),
        documentation: None,
        shape,
    };
    let mut ir = compile_catalog(&catalog("flat graph"), &[])
        .unwrap()
        .ir()
        .clone();
    let leaf = WireTypeId("leaf".into());
    ir.types = (0..DEPTH)
        .map(|index| {
            node(
                format!("chain:{index:06}"),
                WireShape::List {
                    item: if index + 1 == DEPTH {
                        leaf.clone()
                    } else {
                        WireTypeId(format!("chain:{:06}", index + 1))
                    },
                },
            )
        })
        .collect();
    ir.types.push(node("leaf".into(), WireShape::String));
    ir.types.push(node(
        "root".into(),
        WireShape::Record {
            fields: vec![WireField {
                id: "input:value".into(),
                name: "value".into(),
                type_id: WireTypeId("chain:000000".into()),
                optional: false,
                source: source.clone(),
                documentation: None,
            }],
            additional: None,
            nominal: None,
        },
    ));
    ir.operations[0].input = WireTypeId("root".into());
    ir.operations[0].output = CodegenOutput::Value {
        type_id: leaf,
        mcp_envelope: crate::tool_registry::McpOutputEnvelope::Bare,
    };
    ir.required_features = vec![CodegenFeature::RequestCancellation];
    let result = validate_ir(&serde_json::to_vec(&ir).unwrap());
    assert_eq!(
        result.unwrap_err().code,
        CodegenDiagnosticCode::InvalidDigest
    );
    for node in &mut ir.types {
        if let WireShape::List { item } = &node.shape {
            node.shape = WireShape::Alias {
                target: item.clone(),
                declaration: crate::wire::WireDeclarationIdentity {
                    declaration_id: format!("alias:{}", node.id.0),
                    name: "Alias".into(),
                    arguments: Vec::new(),
                },
            };
        }
    }
    assert_eq!(
        validate_ir(&serde_json::to_vec(&ir).unwrap())
            .unwrap_err()
            .code,
        CodegenDiagnosticCode::InvalidDigest
    );
}

#[test]
fn repeated_harn_nominals_share_the_owning_type_identity_across_operation_phases() {
    let source = "pub struct Item { value: int }\npub fn roundtrip(left: Item, right: Item) -> Item { return left }";
    let program = harn_parser::parse_source(source).unwrap();
    let resolver = crate::TypeSchemaResolver::from_program(&program);
    let harn_parser::Node::FnDecl {
        params,
        return_type,
        ..
    } = &program[1].node
    else {
        panic!("function fixture");
    };
    let mut raw = catalog("identity").catalog().clone();
    raw.tools[0].input_schema = resolver.json_schema_for_typed_params(params);
    raw.tools[0].output_schema = resolver.json_schema_for_type_expr(return_type.as_ref().unwrap());
    let prepared = PreparedToolCatalog::prepare(raw).unwrap();
    let ir = compile_catalog(&prepared, &[]).unwrap();
    let nominal: Vec<_> = ir.ir().types.iter().filter(|ty| matches!(
        &ty.shape, crate::wire::WireShape::Record { nominal: Some(nominal), .. } if nominal.name == "Item"
    )).collect();
    assert_eq!(
        nominal.len(),
        1,
        "one Harn declaration has one portable identity"
    );
    validate_ir(&ir.canonical_json()).unwrap();
}

#[test]
fn json_uniqueness_does_not_invent_a_harn_set_or_override_a_wire_type() {
    for schema in [
        json!({"type":"array","items":{"type":"string"},"uniqueItems":true}),
        json!({"type":"integer","x-harn-type":"bytes"}),
        json!({"type":"integer","x-harn-type":"decimal"}),
        json!({"type":"integer","x-harn-type":"set"}),
        json!({"type":"boolean","minimum":1}),
        json!({"const":18446744073709551615_u64}),
        json!({"type":"integer","const":"not an integer"}),
        json!({"oneOf":[{"type":"integer"},{"type":"boolean"}],"type":"string"}),
    ] {
        let mut raw = catalog("schema boundary").catalog().clone();
        raw.tools[0].output_schema = Some(schema);
        if let Ok(prepared) = PreparedToolCatalog::prepare(raw) {
            assert!(compile_catalog(&prepared, &[]).is_err());
        }
    }
}

#[test]
fn native_handshake_requires_all_identity_capability_and_discovery_evidence() {
    use crate::tool_registry::{NativeMcpBindingProfile, NativeMcpBindingViolation};
    let ir = compile_catalog(&catalog("native"), &[]).unwrap();
    let concurrency = std::num::NonZeroU32::new(2).unwrap();
    let profile = NativeMcpBindingProfile::for_ir(&ir, concurrency);
    profile
        .verify(&ir, concurrency, &ir.ir().mcp_tools)
        .unwrap();
    for field in 0..3 {
        let mut wrong = profile.clone();
        match field {
            0 => wrong.identity.catalog_digest.push('0'),
            1 => wrong.identity.binding_digest.push('0'),
            _ => wrong.identity.ir_digest.push('0'),
        }
        assert_eq!(
            wrong.verify(&ir, concurrency, &ir.ir().mcp_tools),
            Err(NativeMcpBindingViolation::Identity)
        );
    }
    let mut wrong = profile.clone();
    wrong.operations.push(wrong.operations[0].clone());
    assert_eq!(
        wrong.verify(&ir, concurrency, &ir.ir().mcp_tools),
        Err(NativeMcpBindingViolation::OperationCensus)
    );
    wrong = profile.clone();
    wrong.max_concurrency = 0;
    assert_eq!(
        wrong.verify(&ir, concurrency, &ir.ir().mcp_tools),
        Err(NativeMcpBindingViolation::Concurrency)
    );
    assert_eq!(
        profile.verify(&ir, concurrency, &[]),
        Err(NativeMcpBindingViolation::Discovery)
    );
    let mut tools = ir.ir().mcp_tools.clone();
    tools[0]["inputSchema"] = json!({});
    assert_eq!(
        profile.verify(&ir, concurrency, &tools),
        Err(NativeMcpBindingViolation::Discovery)
    );
}

#[test]
fn rehashed_forged_ir_cannot_change_the_committed_catalog_graph_or_projection() {
    let original = compile_catalog(&catalog("test"), &[]).unwrap().into_ir();
    for alter in [0, 1] {
        let mut forged = original.clone();
        if alter == 0 {
            let integer = forged
                .types
                .iter_mut()
                .find(|ty| matches!(ty.shape, crate::wire::WireShape::Integer { .. }))
                .unwrap();
            if let crate::wire::WireShape::Integer { maximum, .. } = &mut integer.shape {
                *maximum = "1".into();
            }
        } else {
            forged.mcp_tools[0]["inputSchema"] = json!({"type":"object"});
        }
        forged.identity = identity::expected_identity(&forged).unwrap();
        let failure = validate_ir(&serde_json::to_vec(&forged).unwrap())
            .expect_err("catalog commitment refuses a coherently rehashed forgery");
        assert_eq!(failure.code, CodegenDiagnosticCode::InvalidIr);
    }
}

#[test]
fn documentation_changes_full_identity_without_erasing_a_field_named_description() {
    let before = compile_catalog(&catalog("before"), &[]).unwrap();
    let after = compile_catalog(&catalog("after"), &[]).unwrap();
    assert_ne!(
        before.ir().identity.catalog_digest,
        after.ir().identity.catalog_digest
    );
    assert_ne!(
        before.ir().identity.ir_digest,
        after.ir().identity.ir_digest
    );
    assert_eq!(
        before.ir().identity.binding_digest,
        after.ir().identity.binding_digest
    );
    let mut changed = catalog("before").catalog().clone();
    changed.tools[0].input_schema["properties"]["description"]["maximum"] = json!(5);
    let changed = compile_catalog(&PreparedToolCatalog::prepare(changed).unwrap(), &[]).unwrap();
    assert_ne!(
        before.ir().identity.binding_digest,
        changed.ir().identity.binding_digest
    );
}

#[test]
fn missing_output_is_refused_only_when_selected() {
    let mut source = catalog("test").catalog().clone();
    let mut missing = source.tools[0].clone();
    missing.name = "undocumented".into();
    missing.cli.command = vec!["undocumented".into()];
    missing.output_schema = None;
    source.tools.push(missing);
    let prepared = PreparedToolCatalog::prepare(source).unwrap();
    assert_eq!(
        compile_catalog(&prepared, &[]).unwrap_err().code,
        CodegenDiagnosticCode::MissingOutput
    );
    assert!(compile_catalog(&prepared, &["constructor".into()]).is_ok());
    assert_eq!(
        compile_catalog(&prepared, &["absent".into()])
            .unwrap_err()
            .code,
        CodegenDiagnosticCode::InvalidSelection
    );
}

#[test]
fn plugin_response_identity_features_and_utf8_mappings_are_checked_before_artifacts() {
    let ir = compile_catalog(&catalog("test"), &[]).unwrap().into_ir();
    let request = CodegenRequest {
        schema_version: CodegenRequestVersion::V1,
        role: CodegenRole::Both,
        ir,
    };
    let response = CodegenResponse {
        schema_version: CodegenResponseVersion::V1,
        identity: request.ir.identity.clone(),
        supported_features: request.ir.required_features.clone(),
        artifacts: vec![GeneratedArtifact {
            path: "client.ts".into(),
            content: "é".into(),
            source_mappings: vec![GeneratedRange {
                start: 0,
                end: 2,
                source: request.ir.package.source.clone(),
            }],
        }],
    };
    assert_eq!(
        validate_response(&request, response.clone()).unwrap().len(),
        1
    );
    let mut wrong = response.clone();
    wrong.identity.ir_digest.push('0');
    assert_eq!(
        validate_response(&request, wrong).unwrap_err().code,
        CodegenDiagnosticCode::ResponseIdentityMismatch
    );
    let mut missing = response.clone();
    missing.supported_features.clear();
    assert_eq!(
        validate_response(&request, missing).unwrap_err().code,
        CodegenDiagnosticCode::UnsupportedFeature
    );
    let mut split = response;
    split.artifacts[0].source_mappings[0].end = 1;
    assert_eq!(
        validate_response(&request, split).unwrap_err().code,
        CodegenDiagnosticCode::InvalidSourceMapping
    );
}

#[test]
fn malformed_unknown_and_unmeasured_ir_never_validate() {
    let valid = compile_catalog(&catalog("test"), &[]).unwrap();
    let mut value: Value = serde_json::from_slice(&valid.canonical_json()).unwrap();
    value["extra"] = Value::Null;
    assert_eq!(
        validate_ir(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code,
        CodegenDiagnosticCode::InvalidIr
    );
    assert_eq!(
        validate_ir(b"{}").unwrap_err().code,
        CodegenDiagnosticCode::UnsupportedVersion
    );
    assert_eq!(
        validate_ir(b"{").unwrap_err().code,
        CodegenDiagnosticCode::InvalidJson
    );
}
