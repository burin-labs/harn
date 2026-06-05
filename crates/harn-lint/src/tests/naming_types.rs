//! `naming-convention` plus `unused-type` struct checks.

use super::*;

#[test]
fn test_naming_convention_flags_non_snake_case_function() {
    let diags = lint_source(
        r"
fn BadName() {
  return nil
}
",
    );
    assert!(
        has_rule(&diags, "naming-convention"),
        "expected naming-convention warning, got: {diags:?}"
    );
}

#[test]
fn test_naming_convention_flags_non_pascal_case_type() {
    let diags = lint_source(
        r"
struct bad_name {
  value: int
}
",
    );
    assert!(
        has_rule(&diags, "naming-convention"),
        "expected naming-convention warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_warns_for_unreferenced_struct() {
    let diags = lint_source(
        r#"
struct Helper {
  value: int
}

pipeline default(task) {
  log("ready")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-type"),
        "expected unused-type warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_referenced_struct() {
    let diags = lint_source(
        r"
struct Helper {
  value: int
}

fn build() -> Helper {
  return Helper { value: 1 }
}

pipeline default(task) {
  let item = build()
  log(item.value)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "referenced types should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_warns_for_unreferenced_alias() {
    let diags = lint_source(
        r#"
type Payload = {value: int}

pipeline default(task) {
  log("ready")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-type"),
        "expected unused-type warning for alias, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_referenced_alias() {
    let diags = lint_source(
        r"
type Payload = {value: int}

fn build() -> Payload {
  return {value: 1}
}

pipeline default(task) {
  let item = build()
  log(item.value)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "referenced aliases should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_binding_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  let item: Payload = {value: 1}
  log(item.value)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by binding annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_pipeline_return_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) -> Payload {
  return {value: 1}
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by pipeline return annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_closure_param_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  let read_value = { item: Payload -> item.value }
  log(read_value({value: 1}))
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by closure parameter annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_function_call_type_arg_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

fn identity<T>(item: T) -> T {
  return item
}

pipeline default(task) {
  let item = identity<Payload>({value: 1})
  log(item.value)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by call type arguments should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_schema_of_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  let schema = schema_of(Payload)
  log(schema)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by schema_of(T) should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_typed_catch_reference() {
    let diags = lint_source(
        r#"
type AppError = {message: string}

pipeline default(task) {
  try {
    throw {message: "boom"}
  } catch (err: AppError) {
    log(err.message)
  }
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by typed catch clauses should not trigger unused-type: {diags:?}"
    );
}
