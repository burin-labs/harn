//! Strict-types boundary checks (`json_parse`, `llm_call`) and cross-module call resolution.

use super::*;

#[test]
fn test_strict_types_json_parse_property_access() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let data = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unvalidated")),
        "expected unvalidated warning, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_direct_chain_access() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  log(json_parse("{}").name)
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("Direct property access")),
        "expected direct access warning, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_schema_expect_clears() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let my_schema = {type: "object", properties: {name: {type: "string"}}}
  let data = json_parse("{}")
  schema_expect(data, my_schema)
  log(data.name)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "expected no unvalidated warning after schema_expect, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_schema_is_if_guard() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let my_schema = {type: "object", properties: {name: {type: "string"}}}
  let data = json_parse("{}")
  if schema_is(data, my_schema) {
log(data.name)
  }
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "expected no unvalidated warning inside schema_is guard, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_shape_annotation_clears() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let data: {name: string, age: int} = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "expected no warning with shape annotation, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_propagation() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let data = json_parse("{}")
  let x = data
  log(x.name)
}"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unvalidated") && w.contains("'x'")),
        "expected propagation warning for x, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_non_boundary_no_warning() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let x = len("hello")
  log(x)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "non-boundary function should not be flagged, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_subscript_access() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let data = json_parse("{}")
  log(data["name"])
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unvalidated")),
        "expected subscript warning, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_disabled_by_default() {
    let diags = check_source(
        r#"pipeline t(task) {
  let data = json_parse("{}")
  log(data.name)
}"#,
    );
    assert!(
        !diags.iter().any(|d| d.message.contains("unvalidated")),
        "strict types should be off by default, got: {diags:?}"
    );
}

#[test]
fn test_strict_types_llm_call_without_schema() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let result = llm_call("prompt", "system")
  log(result.text)
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("unvalidated")),
        "llm_call without schema should warn, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_llm_call_with_schema_clean() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let result = llm_call("prompt", "system", {
schema: {type: "object", properties: {name: {type: "string"}}}
  })
  log(result.data)
  log(result.text)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "llm_call with schema should not warn, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_schema_expect_result_typed() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let my_schema = {type: "object", properties: {name: {type: "string"}}}
  let validated = schema_expect(json_parse("{}"), my_schema)
  log(validated.name)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "schema_expect result should be typed, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_realistic_orchestration() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let payload_schema = {type: "object", properties: {
name: {type: "string"},
steps: {type: "list", items: {type: "string"}}
  }}

  // Good: schema-aware llm_call
  let result = llm_call("generate a workflow", "system", {
schema: payload_schema
  })
  let workflow_name = result.data.name

  // Good: validate then access
  let raw = json_parse("{}")
  schema_expect(raw, payload_schema)
  let steps = raw.steps

  log(workflow_name)
  log(steps)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "validated orchestration should be clean, got: {warns:?}"
    );
}

#[test]
fn test_strict_types_llm_call_with_schema_via_variable() {
    let warns = strict_warnings(
        r#"pipeline t(task) {
  let my_schema = {type: "object", properties: {score: {type: "float"}}}
  let result = llm_call("rate this", "system", {
schema: my_schema
  })
  log(result.data.score)
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("unvalidated")),
        "llm_call with schema variable should not warn, got: {warns:?}"
    );
}

#[test]
fn test_cross_module_unresolved_call_errors() {
    let diags = check_source_with_imports(
        r#"pipeline t(task) { missing_helper() }"#,
        &["other_helper"],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        errs.iter().any(|m| m.contains("missing_helper")),
        "expected undefined-call error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_imported_call_is_allowed() {
    let diags =
        check_source_with_imports(r#"pipeline t(task) { helper_fn(1, 2) }"#, &["helper_fn"]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        !errs.iter().any(|m| m.contains("helper_fn")),
        "imported call should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_local_fn_not_flagged() {
    let diags = check_source_with_imports(
        r#"fn local_fn() { 42 }
pipeline t(task) { local_fn() }"#,
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(errs.is_empty(), "local fn should not error, got: {errs:?}");
}

#[test]
fn test_cross_module_forward_reference_is_allowed() {
    // A pipeline that calls a fn declared *later* in the same file
    // should not trigger the strict cross-module undefined-call
    // check, because top-level names are registered up-front.
    let diags = check_source_with_imports(
        r#"pipeline t(task) { helper() }
fn helper() { 42 }"#,
        &[],
    );
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(
        !errs.iter().any(|m| m.contains("helper")),
        "forward-declared fn should not error, got: {errs:?}"
    );
}

#[test]
fn test_cross_module_builtin_not_flagged() {
    let diags = check_source_with_imports(r#"pipeline t(task) { log("hello") }"#, &[]);
    let errs: Vec<&String> = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| &d.message)
        .collect();
    assert!(errs.is_empty(), "builtin should not error, got: {errs:?}");
}
