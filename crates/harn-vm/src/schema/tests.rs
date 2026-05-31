use std::collections::BTreeMap;

use crate::value::VmValue;

use super::canonicalize::canonicalize_schema_value;
use super::limits::{DEFAULT_SCHEMA_MAX_DEPTH, DEFAULT_SCHEMA_MAX_REF_EXPANSIONS};
use super::transform::{merge_schema_dicts, schema_partial_dict};
use super::validate::{validate_schema_value, ValidationOptions};
use super::{
    schema_assert_param, schema_is_value, schema_result_value, schema_to_openapi_schema_value,
};

fn s(v: &str) -> VmValue {
    VmValue::String(std::sync::Arc::from(v))
}

fn make_dict(pairs: Vec<(&str, VmValue)>) -> BTreeMap<String, VmValue> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn make_vm_dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(make_dict(pairs)))
}

fn make_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

fn assert_schema_error_contains(schema: &VmValue, expected: &str) {
    match canonicalize_schema_value(schema) {
        Ok(value) => panic!("expected schema error containing {expected:?}, got {value:?}"),
        Err(error) => assert!(
            error.contains(expected),
            "expected schema error containing {expected:?}, got {error:?}"
        ),
    }
}

fn deep_properties_schema(depth: usize) -> VmValue {
    let mut schema = make_vm_dict(vec![("type", s("string"))]);
    for _ in 0..depth {
        schema = make_vm_dict(vec![
            ("type", s("dict")),
            ("properties", make_vm_dict(vec![("node", schema)])),
        ]);
    }
    schema
}

fn deep_items_schema(depth: usize) -> VmValue {
    let mut schema = make_vm_dict(vec![("type", s("string"))]);
    for _ in 0..depth {
        schema = make_vm_dict(vec![("type", s("list")), ("items", schema)]);
    }
    schema
}

fn deep_ref_chain_schema(depth: usize) -> VmValue {
    let mut definitions = BTreeMap::new();
    for index in 0..depth {
        let schema = if index + 1 == depth {
            make_vm_dict(vec![("type", s("string"))])
        } else {
            make_vm_dict(vec![(
                "$ref",
                s(&format!("#/definitions/Node{}", index + 1)),
            )])
        };
        definitions.insert(format!("Node{index}"), schema);
    }
    make_vm_dict(vec![
        ("$ref", s("#/definitions/Node0")),
        (
            "definitions",
            VmValue::Dict(std::sync::Arc::new(definitions)),
        ),
    ])
}

fn deep_node_value(depth: usize) -> VmValue {
    let mut value = s("ok");
    for _ in 0..depth {
        value = make_vm_dict(vec![("node", value)]);
    }
    value
}

fn deep_list_value(depth: usize) -> VmValue {
    let mut value = s("ok");
    for _ in 0..depth {
        value = make_list(vec![value]);
    }
    value
}

#[test]
fn normalize_json_schema_types() {
    let schema = make_vm_dict(vec![
        ("type", s("object")),
        (
            "properties",
            make_vm_dict(vec![("name", make_vm_dict(vec![("type", s("string"))]))]),
        ),
    ]);
    let normalized = canonicalize_schema_value(&schema).unwrap();
    let dict = normalized.as_dict().unwrap();
    assert_eq!(dict.get("type").unwrap().display(), "dict");
    let props = dict.get("properties").unwrap().as_dict().unwrap();
    assert_eq!(
        props
            .get("name")
            .unwrap()
            .as_dict()
            .unwrap()
            .get("type")
            .unwrap()
            .display(),
        "string"
    );
}

#[test]
fn direct_self_ref_schema_is_rejected() {
    let schema = make_vm_dict(vec![("$ref", s("#"))]);
    assert_schema_error_contains(&schema, "cyclic schema reference: # -> #");
}

#[test]
fn two_node_ref_cycle_is_rejected() {
    let schema = make_vm_dict(vec![(
        "definitions",
        make_vm_dict(vec![
            ("A", make_vm_dict(vec![("$ref", s("#/definitions/B"))])),
            ("B", make_vm_dict(vec![("$ref", s("#/definitions/A"))])),
        ]),
    )]);

    assert_schema_error_contains(
        &schema,
        "cyclic schema reference: #/definitions/A -> #/definitions/B -> #/definitions/A",
    );
}

#[test]
fn deep_properties_schema_is_rejected_at_depth_limit() {
    let schema = deep_properties_schema(DEFAULT_SCHEMA_MAX_DEPTH + 1);
    assert_schema_error_contains(&schema, "schema depth exceeded (128)");
}

#[test]
fn deep_items_schema_is_rejected_at_depth_limit() {
    let schema = deep_items_schema(DEFAULT_SCHEMA_MAX_DEPTH + 1);
    assert_schema_error_contains(&schema, "schema depth exceeded (128)");
}

#[test]
fn many_refs_are_rejected_at_expansion_limit() {
    let mut properties = BTreeMap::new();
    for index in 0..=DEFAULT_SCHEMA_MAX_REF_EXPANSIONS {
        properties.insert(
            format!("p{index}"),
            make_vm_dict(vec![("$ref", s("#/definitions/String"))]),
        );
    }

    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        ("properties", VmValue::Dict(std::sync::Arc::new(properties))),
        (
            "definitions",
            make_vm_dict(vec![("String", make_vm_dict(vec![("type", s("string"))]))]),
        ),
    ]);

    assert_schema_error_contains(&schema, "schema $ref expansion limit exceeded (256)");
}

#[test]
fn deep_ref_chain_is_rejected_at_depth_limit() {
    let schema = deep_ref_chain_schema(DEFAULT_SCHEMA_MAX_DEPTH + 2);
    assert_schema_error_contains(&schema, "schema depth exceeded (128)");
}

#[test]
fn normal_nested_schema_within_limit_still_passes() {
    let schema = deep_properties_schema(8);
    let data = deep_node_value(8);
    assert!(schema_is_value(&data, &schema).unwrap());
}

#[test]
fn validation_depth_limit_returns_error_without_panicking() {
    let schema = deep_items_schema(DEFAULT_SCHEMA_MAX_DEPTH + 1);
    let data = deep_list_value(DEFAULT_SCHEMA_MAX_DEPTH + 1);
    let result = validate_schema_value(
        &data,
        &schema,
        ValidationOptions {
            apply_defaults: false,
            numeric_compat: false,
        },
    );

    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("schema depth exceeded (128)")),
        "expected depth-limit error, got {:?}",
        result.errors
    );
}

#[test]
fn validation_ref_cycle_returns_error_without_panicking() {
    let schema = make_vm_dict(vec![("$ref", s("#"))]);
    let result = validate_schema_value(
        &s("ok"),
        &schema,
        ValidationOptions {
            apply_defaults: false,
            numeric_compat: false,
        },
    );

    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("cyclic schema reference: # -> #")),
        "expected cyclic-ref error, got {:?}",
        result.errors
    );
}

#[test]
fn runtime_param_schema_cycles_are_rejected() {
    let schema = make_vm_dict(vec![("$ref", s("#"))]);
    let error = schema_assert_param(&s("ok"), "payload", &schema)
        .expect_err("cyclic parameter schema must be rejected");

    assert!(
        error
            .to_string()
            .contains("cyclic schema reference: # -> #"),
        "expected cyclic-ref error, got {error:?}"
    );
}

#[test]
fn validate_additional_properties_false() {
    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        ("additional_properties", VmValue::Bool(false)),
        (
            "properties",
            make_vm_dict(vec![("name", make_vm_dict(vec![("type", s("string"))]))]),
        ),
    ]);
    let result = schema_result_value(
        &make_vm_dict(vec![("name", s("Ada")), ("extra", s("x"))]),
        &schema,
        false,
    );
    assert!(matches!(
        result,
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err")
    ));
}

#[test]
fn validate_union_type_array_input() {
    let schema = make_vm_dict(vec![("type", make_list(vec![s("string"), s("integer")]))]);
    assert!(schema_is_value(&VmValue::Int(4), &schema).unwrap());
    assert!(schema_is_value(&s("ok"), &schema).unwrap());
    assert!(!schema_is_value(&VmValue::Bool(true), &schema).unwrap());
}

#[test]
fn all_of_still_applies_sibling_constraints() {
    let schema = make_vm_dict(vec![
        (
            "all_of",
            make_list(vec![make_vm_dict(vec![("type", s("string"))])]),
        ),
        ("min_length", VmValue::Int(3)),
    ]);

    assert!(schema_is_value(&s("abc"), &schema).unwrap());
    assert!(!schema_is_value(&s("ab"), &schema).unwrap());
}

#[test]
fn union_still_applies_sibling_constraints() {
    let schema = make_vm_dict(vec![
        (
            "union",
            make_list(vec![
                make_vm_dict(vec![("type", s("string"))]),
                make_vm_dict(vec![("type", s("int"))]),
            ]),
        ),
        ("enum", make_list(vec![s("allowed"), VmValue::Int(7)])),
    ]);

    assert!(schema_is_value(&s("allowed"), &schema).unwrap());
    assert!(schema_is_value(&VmValue::Int(7), &schema).unwrap());
    assert!(!schema_is_value(&s("blocked"), &schema).unwrap());
}

#[test]
fn export_openapi_nullable() {
    let schema = make_vm_dict(vec![
        ("type", s("string")),
        ("nullable", VmValue::Bool(true)),
    ]);
    let exported = schema_to_openapi_schema_value(&schema).unwrap();
    let dict = exported.as_dict().unwrap();
    assert_eq!(dict.get("type").unwrap().display(), "string");
    assert_eq!(dict.get("nullable").unwrap().display(), "true");
}

#[test]
fn schema_partial_removes_required_recursively() {
    let schema = make_dict(vec![
        ("type", s("dict")),
        ("required", make_list(vec![s("nested")])),
        (
            "properties",
            make_vm_dict(vec![(
                "nested",
                make_vm_dict(vec![
                    ("type", s("dict")),
                    ("required", make_list(vec![s("x")])),
                    (
                        "properties",
                        make_vm_dict(vec![("x", make_vm_dict(vec![("type", s("int"))]))]),
                    ),
                ]),
            )]),
        ),
    ]);
    let partial = schema_partial_dict(&schema);
    assert!(!partial.contains_key("required"));
    let nested = partial
        .get("properties")
        .unwrap()
        .as_dict()
        .unwrap()
        .get("nested")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(nested.get("required").is_none());
}

#[test]
fn merge_schema_dicts_basic() {
    let base = make_dict(vec![("type", s("dict")), ("title", s("Base"))]);
    let overrides = make_dict(vec![("title", s("Override")), ("extra", s("yes"))]);
    let merged = merge_schema_dicts(&base, &overrides);
    assert_eq!(merged.get("type").unwrap().display(), "dict");
    assert_eq!(merged.get("title").unwrap().display(), "Override");
    assert_eq!(merged.get("extra").unwrap().display(), "yes");
}

#[test]
fn pattern_validation_accepts_and_rejects_consistently() {
    // Exercises the cached-pattern path with repeated validations to make
    // sure the cache returns equivalent results to a fresh compile.
    let schema = make_vm_dict(vec![("type", s("string")), ("pattern", s(r"^[a-z]+\d+$"))]);
    for _ in 0..3 {
        assert!(schema_is_value(&s("abc123"), &schema).unwrap());
        assert!(!schema_is_value(&s("ABC123"), &schema).unwrap());
        assert!(!schema_is_value(&s("abc"), &schema).unwrap());
    }
}

#[test]
fn invalid_pattern_surfaces_a_clear_error() {
    let schema = make_vm_dict(vec![
        ("type", s("string")),
        // An unclosed character class is rejected at compile time. The
        // cache stores the error so we don't recompile every call.
        ("pattern", s("[unclosed")),
    ]);
    let result = schema_result_value(&s("anything"), &schema, false);
    let VmValue::EnumVariant(enum_variant) = result else {
        panic!("expected Result variant");
    };
    assert!(enum_variant.is_variant("Result", "Err"));
    let payload_dict = enum_variant
        .fields
        .first()
        .and_then(|value| value.as_dict().cloned())
        .expect("Err payload is a dict");
    let errors = match payload_dict.get("errors") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("expected errors list, got {other:?}"),
    };
    assert!(
        errors
            .iter()
            .any(|err| err.display().contains("invalid regex pattern")),
        "expected an invalid regex error, got: {errors:?}"
    );
    // Calling again hits the cached error path and must produce the same
    // error rather than panicking on a re-compile.
    let _ = schema_result_value(&s("anything"), &schema, false);
}
