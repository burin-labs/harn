//! `unused-function` rule coverage, including call-graph walk,
//! impl-method exemption, recursion handling, and cross-file import
//! suppression.

use super::*;

#[rstest::rstest]
#[case::test_used_function_no_warning(
    r"
pipeline default(task) {
fn helper() {
    return 42
}
log(helper())
}
",
    "unused-function",
    false
)]
#[case::test_pub_function_exempt(
    r#"
/// Documented public function.
pub fn api_endpoint() {
return "ok"
}
"#,
    "unused-function",
    false
)]
#[case::test_function_passed_as_value(
    r"
pipeline default(task) {
fn transformer(x) {
    return x * 2
}
const f = transformer
log(f(5))
}
",
    "unused-function",
    false
)]
#[case::test_function_called_from_another_function(
    r"
pipeline default(task) {
fn inner() {
    return 42
}
fn outer() {
    return inner()
}
log(outer())
}
",
    "unused-function",
    false
)]
#[case::test_pipeline_not_flagged_as_unused(
    r#"
pipeline default(task) {
log("hello")
}
"#,
    "unused-function",
    false
)]
#[case::test_impl_methods_exempt(
    r"
pipeline default(task) {
struct Point {
    x: int
    y: int
}
impl Point {
    fn distance(self) {
        return self.x + self.y
    }
}
const p = Point({x: 3, y: 4})
log(p)
}
",
    "unused-function",
    false
)]
#[case::test_recursive_function_called_externally(
    r"
pipeline default(task) {
fn factorial(n) {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}
log(factorial(5))
}
",
    "unused-function",
    false
)]
#[case::test_mutually_recursive_functions_one_called(
    r"
pipeline default(task) {
fn is_even(n) {
    if n == 0 { return true }
    return is_odd(n - 1)
}
fn is_odd(n) {
    if n == 0 { return false }
    return is_even(n - 1)
}
log(is_even(4))
}
",
    "unused-function",
    false
)]
#[case::test_underscore_prefixed_function_exempt(
    r#"
pipeline default(task) {
fn _unused_helper() {
    return 42
}
log("hello")
}
"#,
    "unused-function",
    false
)]
#[case::test_top_level_unused_function(
    r#"
fn orphan() {
return 42
}
pipeline default(task) {
log("hello")
}
"#,
    "unused-function",
    true
)]
// Wildcard imports shouldn't suppress unused-function checks —
// external code can't call local non-pub functions.
#[case::test_unused_function_with_wildcard_import(
    r#"
import "some_module"
pipeline default(task) {
fn helper() { return 1 }
log("hello")
}
"#,
    "unused-function",
    true
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
fn test_main_function_exempt() {
    let diags = lint_source(
        r"
fn main(_harness: Harness) {
}
",
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "`main` is auto-invoked and should be exempt from unused-function: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "`_harness` should suppress the unused-parameter lint: {diags:?}"
    );
}

#[test]
fn test_unused_function_suggestion_message() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn helper() {
    return 42
}
log("hello")
}
"#,
    );
    let unused = diags
        .iter()
        .find(|d| d.rule == "unused-function")
        .expect("expected unused-function diagnostic");
    assert!(unused.message.contains("helper"));
    assert!(unused.suggestion.as_ref().unwrap().contains("_helper"));
}

#[test]
fn test_multiple_unused_functions() {
    let diags = lint_source(
        r"
pipeline default(task) {
fn helper1() { return 1 }
fn helper2() { return 2 }
fn used() { return 3 }
log(used())
}
",
    );
    assert_eq!(
        count_rule(&diags, "unused-function"),
        2,
        "expected 2 unused-function warnings, got: {diags:?}"
    );
}

#[test]
fn test_unused_function_suppressed_by_cross_file_imports() {
    // When another file imports a function by name, it should not be
    // flagged as unused even if it has no local references.
    let source = r###"
fn done_sentinel() { return "##DONE##" }
fn truly_unused() { return 1 }
"###;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();

    // Without cross-file info: both flagged
    let diags = lint_with_config_and_source(&program, &[], Some(source));
    assert_eq!(
        count_rule(&diags, "unused-function"),
        2,
        "both functions should be flagged without cross-file info: {diags:?}"
    );

    // With cross-file info: only truly_unused flagged
    let mut imported = HashSet::new();
    imported.insert("done_sentinel".to_string());
    let diags = lint_with_cross_file_imports(&program, &[], Some(source), &imported);
    assert_eq!(
        count_rule(&diags, "unused-function"),
        1,
        "only truly_unused should be flagged: {diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.rule == "unused-function" && d.message.contains("truly_unused")),
        "the remaining warning should be for truly_unused: {diags:?}"
    );
}
