//! `missing-stdlib-return-type` (HARN-STD-102) coverage.

use super::*;

#[test]
fn warns_for_public_stdlib_fn_without_return_annotation() {
    let source = "\
pub fn foo() {
  return 1
}
";
    let diags = lint_with_stdlib_return_types(source);
    let missing = diags
        .iter()
        .find(|d| d.rule == "missing-stdlib-return-type")
        .expect("should warn");
    assert_eq!(
        missing.code,
        harn_parser::DiagnosticCode::LintMissingStdlibReturnType
    );
    assert!(
        missing.message.contains("foo"),
        "diagnostic should name the function: {missing:?}"
    );
}

#[test]
fn accepts_public_stdlib_fn_with_return_annotation() {
    let source = "\
pub fn foo() -> int {
  return 1
}
";
    let diags = lint_with_stdlib_return_types(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-return-type"),
        "annotated public functions are already contracted: {diags:?}"
    );
}

#[test]
fn does_not_fire_for_private_helpers() {
    let source = "\
fn helper() {
  return 1
}
";
    let diags = lint_with_stdlib_return_types(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-return-type"),
        "private helpers remain inferable: {diags:?}"
    );
}

#[test]
fn dormant_when_stdlib_return_contract_is_not_required() {
    let source = "\
pub fn foo() {
  return 1
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-return-type"),
        "default lint must not enforce stdlib return contracts: {diags:?}"
    );
}
