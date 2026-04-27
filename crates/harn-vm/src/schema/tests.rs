use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

use super::canonicalize::canonicalize_schema_value;
use super::transform::{merge_schema_dicts, schema_partial_dict};
use super::{schema_is_value, schema_result_value, schema_to_openapi_schema_value};

fn s(v: &str) -> VmValue {
    VmValue::String(Rc::from(v))
}

fn make_dict(pairs: Vec<(&str, VmValue)>) -> BTreeMap<String, VmValue> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn make_vm_dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::Dict(Rc::new(make_dict(pairs)))
}

fn make_list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
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
        VmValue::EnumVariant { variant, .. } if variant.as_ref() == "Err"
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
    let VmValue::EnumVariant {
        variant, fields, ..
    } = result
    else {
        panic!("expected Result variant");
    };
    assert_eq!(variant.as_ref(), "Err");
    let payload_dict = fields
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
