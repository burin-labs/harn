//! `invalid-binary-op-literal` — warnings and string-interpolation autofix.

use super::*;

#[rstest::rstest]
#[case::test_invalid_binary_op_literal_nil(
    r"
pipeline default(task) {
const x = nil - 5
log(x)
}
",
    "invalid-binary-op-literal",
    true
)]
#[case::test_no_invalid_binary_op_for_valid_types(
    r#"
pipeline default(task) {
const x = 1 + 2
const y = "a" + "b"
log(x)
log(y)
}
"#,
    "invalid-binary-op-literal",
    false
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}

#[test]
fn test_fix_invalid_binop_string_plus_bool() {
    let source = "pipeline default(task) {\n  const x = \"hello\" + true\n  log(x)\n}";
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
    let source = "pipeline default(task) {\n  const x = true + 1\n  log(x)\n}";
    let diags = lint_source(source);
    // Without this, a build that stopped flagging the expression would also
    // offer no fix and pass.
    assert!(
        has_rule(&diags, "invalid-binary-op-literal"),
        "expected invalid-binary-op-literal for bool in arithmetic: {diags:?}"
    );
    let fix = get_fix(&diags, "invalid-binary-op-literal");
    assert!(
        fix.is_none(),
        "should not offer fix for non-string binop, got: {fix:?}"
    );
}
