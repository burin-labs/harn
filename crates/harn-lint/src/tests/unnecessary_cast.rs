//! `unnecessary-cast` — flags conversion-builtin calls whose argument is
//! already syntactically of the target type, plus chained identity
//! conversions like `to_string(to_string(x))`. Includes guards against
//! firing on legitimate conversions (`to_int("42")`, `to_float(5)`).

use super::*;

#[rstest::rstest]
#[case::warn_to_float_on_float_literal(
    r"
pipeline default(task) {
  const n = to_float(1.5)
  log(n)
}
",
    "unnecessary-cast",
    true
)]
#[case::warn_to_dict_on_dict_literal(
    r"
pipeline default(task) {
  const d = to_dict({a: 1, b: 2})
  log(d)
}
",
    "unnecessary-cast",
    true
)]
#[case::warn_to_string_on_interpolated_string(
    r#"
pipeline default(task) {
  const name = "world"
  const s = to_string("hello ${name}")
  log(s)
}
"#,
    "unnecessary-cast",
    true
)]
#[case::no_warn_on_genuine_int_to_string(
    r"
pipeline default(task) {
  const n = 5
  const s = to_string(n)
  log(s)
}
",
    "unnecessary-cast",
    false
)]
// `to_int("42")` is a real parse, not a cast. Must not fire.
#[case::no_warn_on_string_to_int(
    r#"
pipeline default(task) {
  const n = to_int("42")
  log(n)
}
"#,
    "unnecessary-cast",
    false
)]
// `to_float(5)` widens int to float. Not a no-op.
#[case::no_warn_on_int_to_float(
    r"
pipeline default(task) {
  const n = to_float(5)
  log(n)
}
",
    "unnecessary-cast",
    false
)]
// `to_int(1.5)` truncates. Not a no-op.
#[case::no_warn_on_float_to_int(
    r"
pipeline default(task) {
  const n = to_int(1.5)
  log(n)
}
",
    "unnecessary-cast",
    false
)]
// `to_list(set(...))` materializes a set as a list. Real conversion.
#[case::no_warn_on_to_list_of_set_call(
    r"
pipeline default(task) {
  const xs = to_list(set([1, 2, 3]))
  log(xs)
}
",
    "unnecessary-cast",
    false
)]
// Defensive: a wrong-arity call is not a cast — leave it alone.
#[case::no_warn_on_zero_or_multi_arg_calls(
    r"
pipeline default(task) {
  const s = to_string()
  const t = to_string(1, 2)
  log(s)
  log(t)
}
",
    "unnecessary-cast",
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
fn fix_to_string_on_string_literal() {
    let source = "pipeline default(task) {\n  const s = to_string(\"hello\")\n  log(s)\n}";
    let diags = lint_source(source);
    assert!(
        get_fix(&diags, "unnecessary-cast").is_some(),
        "expected autofix, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const s = \"hello\""),
        "expected `const s = \"hello\"`, got: {result}"
    );
    assert!(
        !result.contains("to_string"),
        "to_string should be removed, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn fix_to_int_on_int_literal() {
    let source = "pipeline default(task) {\n  const n = to_int(42)\n  log(n)\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const n = 42"),
        "expected `const n = 42`, got: {result}"
    );
}

#[test]
fn fix_to_list_on_list_literal_preserves_inner_formatting() {
    let source = "pipeline default(task) {\n  const xs = to_list([1, 2, 3])\n  log(xs)\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const xs = [1, 2, 3]"),
        "expected list literal with original spacing preserved, got: {result}"
    );
}

#[test]
fn fix_chained_to_string_collapses_one_layer() {
    // Both calls are flagged (outer collapses to inner; if the lint
    // re-runs on the result the now-outer call is no longer redundant
    // because its argument is a bare identifier). The right-to-left,
    // overlap-dropping fix application keeps only the outer one.
    let source =
        "pipeline default(task) {\n  const n = 5\n  const s = to_string(to_string(n))\n  log(s)\n}";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "unnecessary-cast"),
        "expected unnecessary-cast, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const s = to_string(n)"),
        "expected outer to_string removed, got: {result}"
    );
}
