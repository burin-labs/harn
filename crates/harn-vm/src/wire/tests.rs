use harn_parser::TypeExpr;
use serde_json::json;

use super::*;

fn prepared(source: &str, name: &str) -> PreparedWireType {
    let program = harn_parser::parse_source(source).expect("parse");
    crate::TypeSchemaResolver::from_program(&program)
        .prepare_wire_type(
            &TypeExpr::Named(name.into()),
            WireSourceRef::TextSpan {
                document: "fixture.harn".into(),
                start: 0,
                end: source.len() as u32,
            },
        )
        .expect("prepared declared wire type")
}

#[test]
fn graph_cycle_detection_distinguishes_shared_edges_and_later_components() {
    let node = |id: &str, shape| WireType {
        id: WireTypeId(id.into()),
        name: None,
        source: WireSourceRef::Synthetic {
            owner: "cycle-query".into(),
        },
        documentation: None,
        shape,
    };
    let mut types = vec![
        node(
            "a",
            WireShape::Tuple {
                items: vec![WireTypeId("b".into()), WireTypeId("c".into())],
            },
        ),
        node(
            "b",
            WireShape::List {
                item: WireTypeId("c".into()),
            },
        ),
        node("c", WireShape::String),
    ];
    assert!(!PreparedWireGraph::prepare(types.clone())
        .unwrap()
        .has_recursive_types());
    types.push(node(
        "later",
        WireShape::List {
            item: WireTypeId("later".into()),
        },
    ));
    assert!(PreparedWireGraph::prepare(types)
        .unwrap()
        .has_recursive_types());
}

#[test]
fn alias_preparation_retains_dangling_and_alias_only_cycle_refusals() {
    let alias = |id: &str, target: &str| WireType {
        id: WireTypeId(id.into()),
        name: None,
        source: WireSourceRef::Synthetic {
            owner: "alias-query".into(),
        },
        documentation: None,
        shape: WireShape::Alias {
            target: WireTypeId(target.into()),
            declaration: WireDeclarationIdentity {
                declaration_id: id.into(),
                name: id.into(),
                arguments: Vec::new(),
            },
        },
    };
    assert_eq!(
        PreparedWireGraph::prepare([alias("a", "missing")])
            .unwrap_err()
            .code,
        WireFailureCode::UnresolvedReference
    );
    assert_eq!(
        PreparedWireGraph::prepare([alias("a", "b"), alias("b", "a")])
            .unwrap_err()
            .code,
        WireFailureCode::RecursiveExpansion
    );
}

#[test]
fn recursive_nominals_hydrate_before_handler_and_preserve_missing_nullable_fields() {
    let wire = prepared(
        "pub struct Node { value: int\n next?: Node | nil\n }",
        "Node",
    );
    let input = json!({"value":9007199254740993_i64,"next":{"value":2,"next":null}});
    let value = wire.decode(&input).expect("decode nominal recursively");
    assert_eq!(value.struct_name(), Some("Node"));
    assert_eq!(
        value.struct_field("next").unwrap().struct_name(),
        Some("Node")
    );
    assert_eq!(wire.encode(&value).unwrap(), input);
    let absent = wire.decode(&json!({"value":1})).unwrap();
    assert!(absent.struct_field("next").is_none());
    assert_eq!(wire.encode(&absent).unwrap(), json!({"value":1}));
    assert!(wire
        .decode(&json!({"value":1,"next":{"value":"secret"}}))
        .is_err());
}

#[test]
fn resolved_alias_intersections_merge_closed_fields_without_losing_optional_constraints() {
    let wire = prepared(
        "pub type Left = { left: int, shared?: string }\npub type Right = { right: bool, shared: string }\npub type Both = Left & Right",
        "Both",
    );
    let value = json!({"left":1,"right":true,"shared":"present"});
    assert_eq!(wire.encode(&wire.decode(&value).unwrap()).unwrap(), value);
    assert!(wire.decode(&json!({"left":1,"right":true})).is_err());
    assert!(wire
        .decode(&json!({"left":1,"right":true,"shared":null}))
        .is_err());
    assert!(wire
        .decode(&json!({"left":1,"right":true,"shared":"present","extra":1}))
        .is_err());
}

#[test]
fn integer_wire_tokens_never_coerce_rounded_floats_or_negative_zero() {
    let integer = crate::TypeSchemaResolver::empty()
        .prepare_wire_type(
            &TypeExpr::Named("int".into()),
            WireSourceRef::Synthetic {
                owner: "integer tokens".into(),
            },
        )
        .unwrap();
    let pattern = regex::Regex::new(WIRE_INTEGER_TOKEN_PATTERN).unwrap();
    for raw in [
        "0",
        "1",
        "-1",
        "9007199254740993",
        "9223372036854775807",
        "-9223372036854775808",
    ] {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(pattern.is_match(raw));
        assert_eq!(
            integer.encode(&integer.decode(&value).unwrap()).unwrap(),
            value
        );
    }
    for raw in [
        "-0",
        "1.0",
        "1e0",
        "9007199254740993.0",
        "0.99999999999999999",
    ] {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(!pattern.is_match(raw));
        assert!(
            integer.decode(&value).is_err(),
            "refuse numeric spelling {raw}"
        );
    }
    for raw in ["9223372036854775808", "-9223372036854775809"] {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(integer.decode(&value).is_err());
    }
}

#[test]
fn float_wire_retains_existing_integer_numeric_widening() {
    let float = crate::TypeSchemaResolver::empty()
        .prepare_wire_type(
            &TypeExpr::Named("float".into()),
            WireSourceRef::Synthetic {
                owner: "float widening".into(),
            },
        )
        .unwrap();
    for token in [
        "1",
        "1.0",
        "1e0",
        "-0",
        "9007199254740993",
        "9223372036854775807",
    ] {
        let json: serde_json::Value = serde_json::from_str(token).unwrap();
        let value = float.decode(&json).unwrap();
        let crate::VmValue::Float(value) = value else {
            panic!("float decoder must hydrate a float")
        };
        assert_eq!(value.to_bits(), json.as_f64().unwrap().to_bits());
    }
    assert_eq!(float.encode(&crate::VmValue::Int(1)).unwrap(), json!(1));
}

#[test]
fn empty_enum_fields_and_empty_tuples_have_valid_closed_json_schemas() {
    let wire = prepared(
        "pub enum Choice { Missing\n Present(value: int) }",
        "Choice",
    );
    jsonschema::validator_for(&wire.json_schema()).expect("valid draft schema");
    let value = json!({"enum":"Choice","variant":"Missing","fields":[]});
    assert_eq!(wire.encode(&wire.decode(&value).unwrap()).unwrap(), value);
    let tuple = crate::TypeSchemaResolver::empty()
        .prepare_wire_type(
            &TypeExpr::Tuple(Vec::new()),
            WireSourceRef::Synthetic {
                owner: "empty tuple".into(),
            },
        )
        .unwrap();
    jsonschema::validator_for(&tuple.json_schema()).expect("valid empty tuple schema");
    assert_eq!(
        tuple.encode(&tuple.decode(&json!([])).unwrap()).unwrap(),
        json!([])
    );
    assert!(tuple.decode(&json!([0])).is_err());
}

#[test]
fn bytes_decimal_sets_and_enum_are_type_directed_without_magic_in_maps() {
    let wire = prepared(
        "pub enum Payload { Data(raw: bytes, amount: decimal, names: set<string>) }",
        "Payload",
    );
    let input = json!({"enum":"Payload","variant":"Data","fields":[{"$bytes_b64":"AP8="},"1.00",["a","b"]]});
    let value = wire.decode(&input).unwrap();
    let crate::VmValue::EnumVariant(variant) = &value else {
        panic!("nominal runtime value");
    };
    assert!(
        matches!(&variant.fields[0], crate::VmValue::Bytes(bytes) if bytes.as_slice() == [0,255])
    );
    assert!(matches!(&variant.fields[1], crate::VmValue::Decimal(_)));
    assert!(matches!(&variant.fields[2], crate::VmValue::Set(_)));
    assert_eq!(wire.encode(&value).unwrap(), input);
    let map = crate::TypeSchemaResolver::empty()
        .prepare_wire_type(
            &TypeExpr::DictType(
                Box::new(TypeExpr::Named("string".into())),
                Box::new(TypeExpr::Named("string".into())),
            ),
            WireSourceRef::Synthetic {
                owner: "map".into(),
            },
        )
        .unwrap();
    let collision = map.decode(&json!({"$bytes_b64":"AP8="})).unwrap();
    assert!(matches!(collision, crate::VmValue::Dict(_)));
}

#[test]
fn malformed_special_values_and_wrong_nominals_cannot_become_success() {
    let wire = prepared(
        "pub enum Payload { Data(raw: bytes, amount: decimal, names: set<string>) }",
        "Payload",
    );
    let valid = json!({"enum":"Payload","variant":"Data","fields":[{"$bytes_b64":"AP8="},"1.00",["a","b"]]});
    for (path, value, code) in [
        (
            "/fields/0/$bytes_b64",
            json!("AP8"),
            WireFailureCode::InvalidBase64,
        ),
        ("/fields/1", json!("1e0"), WireFailureCode::InvalidDecimal),
        (
            "/fields/2",
            json!(["a", "a"]),
            WireFailureCode::DuplicateSetMember,
        ),
        (
            "/enum",
            json!("Other"),
            WireFailureCode::InvalidNominalDiscriminator,
        ),
    ] {
        let mut malformed = valid.clone();
        *malformed.pointer_mut(path).unwrap() = value;
        assert_eq!(wire.decode(&malformed).unwrap_err().code, code);
    }
    let generic = crate::value::json::json_to_vm_value(&valid);
    assert_eq!(
        wire.encode(&generic).unwrap_err().code,
        WireFailureCode::WrongRuntimeType
    );
}

#[test]
fn overlapping_union_refuses_before_values_while_discriminated_records_work() {
    let resolver = crate::TypeSchemaResolver::empty();
    let source = WireSourceRef::Synthetic {
        owner: "union".into(),
    };
    let ambiguous = TypeExpr::Union(vec![
        TypeExpr::Named("int".into()),
        TypeExpr::Named("float".into()),
    ]);
    assert_eq!(
        resolver
            .prepare_wire_type(&ambiguous, source)
            .unwrap_err()
            .code,
        WireFailureCode::AmbiguousUnion
    );
    let wire = prepared(
        "pub type Choice = {kind: \"left\", value: bytes} | {kind: \"right\", value: string}",
        "Choice",
    );
    let value = json!({"kind":"left","value":{"$bytes_b64":"AP8="}});
    assert_eq!(wire.encode(&wire.decode(&value).unwrap()).unwrap(), value);
}

#[test]
fn optional_does_not_accept_null_without_a_nullable_declared_type() {
    let wire = prepared(
        "pub type Input = {maybe?: string, nullable: string | nil}",
        "Input",
    );
    assert!(wire.decode(&json!({"nullable":null})).is_ok());
    assert_eq!(
        wire.decode(&json!({"nullable":null,"maybe":null}))
            .unwrap_err()
            .code,
        WireFailureCode::InvalidValue
    );
    assert_eq!(
        wire.decode(&json!({})).unwrap_err().code,
        WireFailureCode::MissingField
    );
}
