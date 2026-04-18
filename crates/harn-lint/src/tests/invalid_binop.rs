//! `invalid-binary-op-literal` — warnings and string-interpolation autofix.

use super::*;

#[test]
fn test_invalid_binary_op_literal_bool() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = true + 1
log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "invalid-binary-op-literal"),
        "expected invalid-binary-op-literal for bool in arithmetic: {diags:?}"
    );
}

#[test]
fn test_invalid_binary_op_literal_nil() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = nil - 5
log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "invalid-binary-op-literal"),
        "expected invalid-binary-op-literal for nil in arithmetic: {diags:?}"
    );
}

#[test]
fn test_no_invalid_binary_op_for_valid_types() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1 + 2
let y = "a" + "b"
log(x)
log(y)
}
"#,
    );
    assert!(
        !has_rule(&diags, "invalid-binary-op-literal"),
        "should not fire for valid operand types: {diags:?}"
    );
}

#[test]
fn test_fix_invalid_binop_string_plus_bool() {
    let source = "pipeline default(task) {\n  let x = \"hello\" + true\n  log(x)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "invalid-binary-op-literal");
    assert!(
        fix.is_some(),
        "expected interpolation fix for string + bool"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("\"hello${true}\""),
        "expected interpolation, got: {result}"
    );
}

#[test]
fn test_fix_invalid_binop_no_fix_for_non_string() {
    let source = "pipeline default(task) {\n  let x = true + 1\n  log(x)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "invalid-binary-op-literal");
    assert!(
        fix.is_none(),
        "should not offer fix for non-string binop, got: {fix:?}"
    );
}
