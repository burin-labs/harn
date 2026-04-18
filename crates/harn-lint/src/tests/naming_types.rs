//! `naming-convention` plus `unused-type` struct checks.

use super::*;

#[test]
fn test_naming_convention_flags_non_snake_case_function() {
    let diags = lint_source(
        r#"
fn BadName() {
  return nil
}
"#,
    );
    assert!(
        has_rule(&diags, "naming-convention"),
        "expected naming-convention warning, got: {diags:?}"
    );
}

#[test]
fn test_naming_convention_flags_non_pascal_case_type() {
    let diags = lint_source(
        r#"
struct bad_name {
  value: int
}
"#,
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
        r#"
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
"#,
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "referenced types should not trigger unused-type: {diags:?}"
    );
}
