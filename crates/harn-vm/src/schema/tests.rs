use std::collections::BTreeMap;

use crate::value::VmValue;

use super::canonicalize::canonicalize_schema_value;
use super::limits::{DEFAULT_SCHEMA_MAX_DEPTH, DEFAULT_SCHEMA_MAX_REF_EXPANSIONS};
use super::transform::{merge_schema_dicts, schema_partial_dict};
use super::validate::{validate_schema_value, ValidationOptions};
use super::{
    normalize_provider_json_schema, reject_unsatisfiable_output_schema, schema_assert_param,
    schema_is_value, schema_report_value, schema_result_value, schema_to_json_schema_value,
    schema_to_openapi_schema_value,
};

fn s(v: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(v))
}

fn make_dict(pairs: Vec<(&str, VmValue)>) -> crate::value::DictMap {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn make_vm_dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::dict(make_dict(pairs))
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
        ("definitions", VmValue::dict(definitions)),
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
fn dollar_defs_refs_validate_and_round_trip() {
    let schema = make_vm_dict(vec![
        ("type", s("object")),
        (
            "properties",
            make_vm_dict(vec![(
                "expected_hash",
                make_vm_dict(vec![("$ref", s("#/$defs/sha256Label"))]),
            )]),
        ),
        (
            "$defs",
            make_vm_dict(vec![(
                "sha256Label",
                make_vm_dict(vec![
                    ("type", s("string")),
                    ("pattern", s("^sha256:[0-9a-f]{64}$")),
                ]),
            )]),
        ),
    ]);

    let normalized = canonicalize_schema_value(&schema).unwrap();
    assert!(normalized.as_dict().unwrap().contains_key("$defs"));
    assert!(schema_is_value(
        &make_vm_dict(vec![(
            "expected_hash",
            s("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        )]),
        &normalized,
    )
    .unwrap());
    assert!(!schema_is_value(
        &make_vm_dict(vec![("expected_hash", s("sha256:nothex"))]),
        &normalized,
    )
    .unwrap());

    let exported = schema_to_json_schema_value(&normalized).unwrap();
    let exported_dict = exported.as_dict().unwrap();
    assert!(exported_dict.contains_key("$defs"));
    let expected_hash = exported_dict
        .get("properties")
        .and_then(VmValue::as_dict)
        .and_then(|properties| properties.get("expected_hash"))
        .and_then(VmValue::as_dict)
        .expect("expected_hash property schema");
    assert_eq!(
        expected_hash.get("$ref").map(VmValue::display),
        Some("#/$defs/sha256Label".to_string())
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
        ("properties", VmValue::dict(properties)),
        (
            "definitions",
            make_vm_dict(vec![("String", make_vm_dict(vec![("type", s("string"))]))]),
        ),
    ]);

    assert_schema_error_contains(&schema, "schema $ref expansion limit exceeded (256)");
}

/// A `$ref` item schema costs one expansion per element, which is ordinary work
/// and must not be charged against a document-wide budget.
///
/// Spending the budget across the whole value made validation reject data for
/// being large: past the 256th element every remaining element failed with
/// "schema $ref expansion limit exceeded", naming the data when the data was
/// fine. Observed on a real host response — `run_command` returns
/// `process_cleanup.reaped_children`, an array whose items are a `$ref`, and on
/// a busy machine the whole call threw rather than returning its result.
#[test]
fn a_wide_array_of_ref_items_is_not_charged_a_document_wide_budget() {
    let schema = make_vm_dict(vec![
        ("type", s("list")),
        (
            "items",
            make_vm_dict(vec![("$ref", s("#/definitions/Int"))]),
        ),
        (
            "definitions",
            make_vm_dict(vec![("Int", make_vm_dict(vec![("type", s("int"))]))]),
        ),
    ]);
    let canonical = canonicalize_schema_value(&schema).expect("schema canonicalizes");

    let width = DEFAULT_SCHEMA_MAX_REF_EXPANSIONS * 4;
    let value = make_list((0..width).map(|index| VmValue::Int(index as i64)).collect());
    let report = validate_schema_value(
        &value,
        &canonical,
        ValidationOptions {
            apply_defaults: false,
        },
    );
    assert!(
        report.errors.is_empty(),
        "a {width}-element array of ints should validate, got {:?}",
        report.errors
    );

    // The budget still has to catch a genuinely bad element anywhere in the
    // array, so the fix must not have turned validation off past element 256.
    let mut mixed = (0..width)
        .map(|index| VmValue::Int(index as i64))
        .collect::<Vec<_>>();
    mixed[width - 1] = s("not an int");
    let report = validate_schema_value(
        &make_list(mixed),
        &canonical,
        ValidationOptions {
            apply_defaults: false,
        },
    );
    assert_eq!(
        report.errors.len(),
        1,
        "the last element is still checked: {:?}",
        report.errors
    );
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
        },
    );

    assert!(
        result
            .errors
            .iter()
            .any(|error| error.message.contains("schema depth exceeded (128)")),
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
        },
    );

    assert!(
        result
            .errors
            .iter()
            .any(|error| error.message.contains("cyclic schema reference: # -> #")),
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

/// The request side of the same budget.
///
/// The schema here holds a single `$ref`; the breadth is in the *value*, which
/// is where a caller's data lives. Each field costs one expansion, so charging
/// them to one budget rejected a parameter for having many keys.
#[test]
fn a_wide_object_param_is_not_charged_a_document_wide_budget() {
    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        (
            "additional_properties",
            make_vm_dict(vec![("$ref", s("#/definitions/Str"))]),
        ),
        (
            "definitions",
            make_vm_dict(vec![("Str", make_vm_dict(vec![("type", s("string"))]))]),
        ),
    ]);

    let width = DEFAULT_SCHEMA_MAX_REF_EXPANSIONS * 2;
    let mut fields = BTreeMap::new();
    for index in 0..width {
        fields.insert(format!("f{index:04}"), s("ok"));
    }

    schema_assert_param(&VmValue::dict(fields.clone()), "payload", &schema)
        .expect("a wide object parameter should validate");

    // And a wrong field past the old cutoff is still reported.
    let last = format!("f{:04}", width - 1);
    fields.insert(last.clone(), VmValue::Int(7));
    let error = schema_assert_param(&VmValue::dict(fields), "payload", &schema)
        .expect_err("the last field is still checked");
    assert!(
        error.to_string().contains(&format!("payload.{last}")),
        "expected the offending field to be named, got {error:?}"
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
fn schema_report_includes_structured_issues() {
    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        (
            "properties",
            make_vm_dict(vec![
                (
                    "age",
                    make_vm_dict(vec![("type", s("int")), ("min", VmValue::Int(0))]),
                ),
                (
                    "name",
                    make_vm_dict(vec![("type", s("string")), ("min_length", VmValue::Int(2))]),
                ),
            ]),
        ),
    ]);
    let report = schema_report_value(
        &make_vm_dict(vec![("age", VmValue::Int(-1)), ("name", s("A"))]),
        &schema,
        false,
    );
    let payload = report.as_dict().expect("schema_report returns a dict");
    assert!(matches!(payload.get("ok"), Some(VmValue::Bool(false))));
    assert!(
        payload.contains_key("value"),
        "failed validation still reports the normalized value"
    );

    let errors = match payload.get("errors") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("expected errors list, got {other:?}"),
    };
    assert_eq!(errors.len(), 2);
    assert!(
        errors.iter().any(|error| {
            let message = error.display();
            message.contains("at age:") && message.contains("minimum")
        }),
        "expected path-aware age error, got: {errors:?}"
    );

    let issues = match payload.get("issues") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("expected issues list, got {other:?}"),
    };
    assert_eq!(issues.len(), 2);
    assert!(issues.iter().any(|issue| {
        issue
            .as_dict()
            .and_then(|dict| dict.get("path"))
            .is_some_and(|path| path.display() == "age")
    }));
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
fn union_accepts_overlapping_branches_and_exports_as_any_of() {
    let branch = make_vm_dict(vec![("type", s("string"))]);
    let schema = make_vm_dict(vec![("union", make_list(vec![branch.clone(), branch]))]);

    assert!(schema_is_value(&s("overlap"), &schema).unwrap());
    let exported = schema_to_json_schema_value(&schema).unwrap();
    let dict = exported.as_dict().expect("exported schema object");
    assert!(dict.contains_key("anyOf"));
    assert!(!dict.contains_key("oneOf"));
}

#[test]
fn union_defaults_come_from_the_matching_same_shape_branch() {
    let branch = |tag: &str, field: &str, default: &str| {
        make_vm_dict(vec![
            ("type", s("dict")),
            (
                "properties",
                make_vm_dict(vec![
                    ("kind", make_vm_dict(vec![("const", s(tag))])),
                    (
                        field,
                        make_vm_dict(vec![("type", s("string")), ("default", s(default))]),
                    ),
                ]),
            ),
            ("required", make_list(vec![s("kind")])),
        ])
    };
    let schema = make_vm_dict(vec![(
        "union",
        make_list(vec![branch("a", "alpha", "A"), branch("b", "beta", "B")]),
    )]);
    let result = validate_schema_value(
        &make_vm_dict(vec![("kind", s("b"))]),
        &schema,
        ValidationOptions {
            apply_defaults: true,
        },
    );

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let fields = result.value.as_dict().expect("defaulted dict");
    assert!(matches!(fields.get("beta"), Some(VmValue::String(value)) if value.as_str() == "B"));
    assert!(!fields.contains_key("alpha"));
}

#[test]
fn enum_constraints_apply_to_collection_values() {
    let list_schema = make_vm_dict(vec![(
        "enum",
        make_list(vec![make_list(vec![VmValue::Int(1), VmValue::Int(2)])]),
    )]);
    assert!(schema_is_value(
        &make_list(vec![VmValue::Int(1), VmValue::Int(2)]),
        &list_schema
    )
    .unwrap());
    assert!(!schema_is_value(
        &make_list(vec![VmValue::Int(2), VmValue::Int(1)]),
        &list_schema
    )
    .unwrap());

    let dict_schema = make_vm_dict(vec![(
        "enum",
        make_list(vec![make_vm_dict(vec![("name", s("Ada"))])]),
    )]);
    assert!(schema_is_value(&make_vm_dict(vec![("name", s("Ada"))]), &dict_schema).unwrap());
    assert!(!schema_is_value(&make_vm_dict(vec![("name", s("Grace"))]), &dict_schema).unwrap());
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
fn export_openapi_nullable_type_union() {
    let schema = make_vm_dict(vec![("type", make_list(vec![s("string"), s("null")]))]);
    let exported = schema_to_openapi_schema_value(&schema).unwrap();
    let dict = exported.as_dict().unwrap();
    assert_eq!(dict.get("type").unwrap().display(), "string");
    assert_eq!(dict.get("nullable").unwrap().display(), "true");
    assert!(!dict.contains_key("oneOf"));
}

#[test]
fn export_json_schema_omits_invalid_any_type() {
    let schema = make_vm_dict(vec![("type", s("any"))]);
    let exported = schema_to_json_schema_value(&schema).unwrap();
    let dict = exported.as_dict().unwrap();
    assert!(!dict.contains_key("type"));
}

#[test]
fn export_json_schema_emits_required_empty_for_object_properties() {
    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        ("properties", make_vm_dict(vec![])),
    ]);

    let exported = schema_to_json_schema_value(&schema).unwrap();
    let dict = exported.as_dict().unwrap();

    assert_eq!(dict.get("type").unwrap().display(), "object");
    assert!(dict
        .get("properties")
        .unwrap()
        .as_dict()
        .unwrap()
        .is_empty());
    assert!(matches!(
        dict.get("required"),
        Some(VmValue::List(items)) if items.is_empty()
    ));
}

#[test]
fn export_json_schema_emits_nested_required_empty() {
    let schema = make_vm_dict(vec![
        ("type", s("dict")),
        (
            "properties",
            make_vm_dict(vec![(
                "options",
                make_vm_dict(vec![
                    ("type", s("dict")),
                    (
                        "properties",
                        make_vm_dict(vec![("limit", make_vm_dict(vec![("type", s("int"))]))]),
                    ),
                ]),
            )]),
        ),
        ("required", make_list(vec![s("options")])),
    ]);

    let exported = schema_to_json_schema_value(&schema).unwrap();
    let options = exported
        .as_dict()
        .and_then(|dict| dict.get("properties"))
        .and_then(VmValue::as_dict)
        .and_then(|properties| properties.get("options"))
        .and_then(VmValue::as_dict)
        .expect("options property schema");

    assert!(matches!(
        options.get("required"),
        Some(VmValue::List(items)) if items.is_empty()
    ));
}

#[test]
fn json_schema_unique_items_validates_lists_without_requiring_harn_set() {
    let schema = make_vm_dict(vec![
        ("type", s("array")),
        ("uniqueItems", VmValue::Bool(true)),
    ]);
    assert!(schema_is_value(&make_list(vec![VmValue::Int(1), VmValue::Int(2)]), &schema).unwrap());
    assert!(!schema_is_value(&make_list(vec![VmValue::Int(1), VmValue::Int(1)]), &schema).unwrap());
}

#[test]
fn json_schema_fractional_multiple_of_accepts_and_rejects_consistently() {
    let schema = make_vm_dict(vec![
        ("type", s("number")),
        ("multipleOf", VmValue::Float(0.25)),
    ]);

    assert!(schema_is_value(&VmValue::Float(1.5), &schema).unwrap());
    assert!(!schema_is_value(&VmValue::Float(1.3), &schema).unwrap());

    let invalid = make_vm_dict(vec![
        ("type", s("number")),
        ("multipleOf", VmValue::Float(0.0)),
    ]);
    assert_schema_error_contains(&invalid, "greater than zero");
}

#[test]
fn export_set_schema_keeps_harn_marker() {
    let schema = make_vm_dict(vec![("type", s("set"))]);
    let exported = schema_to_json_schema_value(&schema).unwrap();
    let dict = exported.as_dict().unwrap();
    assert_eq!(dict.get("type").unwrap().display(), "array");
    assert_eq!(dict.get("uniqueItems").unwrap().display(), "true");
    assert_eq!(dict.get("x-harn-type").unwrap().display(), "set");
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
        errors.iter().any(|err| {
            let message = err.display();
            message.contains("invalid JSON Schema") && message.contains("regex")
        }),
        "expected an invalid regex error, got: {errors:?}"
    );
    // Calling again hits the cached error path and must produce the same
    // error rather than panicking on a re-compile.
    let _ = schema_result_value(&s("anything"), &schema, false);
}

fn edit_tool_call_schema(actions: &[&str]) -> serde_json::Value {
    let branches: Vec<serde_json::Value> = actions
        .iter()
        .map(|action| {
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {"const": action},
                    "path": {"type": "string"}
                },
                "required": ["action", "path"]
            })
        })
        .collect();
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "name": {"type": "string", "enum": ["edit"]},
            "args": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {"union": branches}
                },
                "required": ["action"]
            }
        },
        "required": ["name", "args"]
    })
}

fn collapse_edit_action_branches(
    mut schema: serde_json::Value,
    collapsed: &[&str],
) -> serde_json::Value {
    schema["properties"]["args"]["properties"]["action"] = serde_json::json!({
        "anyOf": [],
        "x-harn-collapsed-branches": collapsed,
    });
    schema
}

fn serialized_union_branch_count(schema: &serde_json::Value) -> usize {
    schema
        .pointer("/properties/args/properties/action/anyOf")
        .or_else(|| schema.pointer("/properties/args/properties/action/oneOf"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// After a rejected `edit` call (missing `action`), the next constrained
/// request must still admit at least one branch — or refuse to emit.
#[test]
fn discriminated_union_tool_after_rejected_call_keeps_or_rejects_empty_schema() {
    let actions = ["create", "replace_range", "append"];
    let mut admissible = edit_tool_call_schema(&actions);
    normalize_provider_json_schema(&mut admissible);
    reject_unsatisfiable_output_schema(&admissible)
        .expect("a live edit union must remain satisfiable");
    assert!(
        serialized_union_branch_count(&admissible) >= 1,
        "serialized schema must keep at least one action branch: {admissible}"
    );

    let mut collapsed = collapse_edit_action_branches(edit_tool_call_schema(&actions), &actions);
    normalize_provider_json_schema(&mut collapsed);
    let error = reject_unsatisfiable_output_schema(&collapsed)
        .expect_err("empty anyOf after branch collapse must be rejected before emit");
    assert_eq!(error.tool.as_deref(), Some("edit"));
    assert_eq!(
        error.collapsed_branches,
        actions.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    assert_eq!(error.path, "#/properties/args/properties/action/anyOf");
    assert_eq!(error.keyword, "anyOf");
}

#[test]
fn empty_union_rewrite_is_rejected_as_unsatisfiable() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": {"enum": ["edit"]},
            "args": {"union": []}
        },
        "required": ["args"]
    });
    normalize_provider_json_schema(&mut schema);
    assert_eq!(schema["properties"]["args"]["anyOf"], serde_json::json!([]));
    let error = reject_unsatisfiable_output_schema(&schema)
        .expect_err("union: [] rewritten to anyOf: [] must not be emitted");
    assert_eq!(error.tool.as_deref(), Some("edit"));
    assert_eq!(error.keyword, "anyOf");
}

#[test]
fn empty_enum_and_impossible_required_are_rejected() {
    let empty_enum = serde_json::json!({"type": "string", "enum": []});
    let error = reject_unsatisfiable_output_schema(&empty_enum).expect_err("empty enum");
    assert_eq!(error.keyword, "enum");

    let impossible = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
        "required": ["action"]
    });
    let error = reject_unsatisfiable_output_schema(&impossible).expect_err("impossible required");
    assert_eq!(error.keyword, "required");
    assert_eq!(error.collapsed_branches, ["action"]);
}

#[test]
fn unsatisfiable_analysis_does_not_reject_live_alternatives() {
    let admissible = [
        serde_json::json!({
            "type": "object",
            "properties": {"optional": false}
        }),
        serde_json::json!({
            "type": "object",
            "$defs": {"unused": false}
        }),
        serde_json::json!({"not": false}),
        serde_json::json!({"if": true, "then": false, "else": false}),
        serde_json::json!({"anyOf": [false, {"type": "string"}]}),
        serde_json::json!({"oneOf": [false, {"type": "string"}]}),
        serde_json::json!({
            "properties": {"required_but_not_object_only": false},
            "required": ["required_but_not_object_only"]
        }),
    ];

    for schema in admissible {
        reject_unsatisfiable_output_schema(&schema)
            .unwrap_or_else(|error| panic!("live schema was rejected: {schema}: {error}"));
    }
}

#[test]
fn unsatisfiable_analysis_proves_only_structural_contradictions() {
    let unsatisfiable = [
        serde_json::json!(false),
        serde_json::json!({"anyOf": [false, {"enum": []}]}),
        serde_json::json!({"oneOf": [false, {"enum": []}]}),
        serde_json::json!({"allOf": [{"type": "string"}, false]}),
        serde_json::json!({
            "type": "object",
            "properties": {"required_child": false},
            "required": ["required_child"]
        }),
    ];

    for schema in unsatisfiable {
        assert!(
            reject_unsatisfiable_output_schema(&schema).is_err(),
            "structural contradiction was admitted: {schema}"
        );
    }
}
