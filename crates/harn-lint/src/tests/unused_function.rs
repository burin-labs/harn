//! `unused-function` rule coverage, including call-graph walk,
//! impl-method exemption, recursion handling, and cross-file import
//! suppression.

use super::*;

#[test]
fn test_unused_function_basic() {
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
    assert!(
        has_rule(&diags, "unused-function"),
        "expected unused-function warning, got: {diags:?}"
    );
}

#[test]
fn test_used_function_no_warning() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn helper() {
    return 42
}
log(helper())
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "used function should not trigger unused-function: {diags:?}"
    );
}

#[test]
fn test_pub_function_exempt() {
    let diags = lint_source(
        r#"
/// Documented public function.
pub fn api_endpoint() {
return "ok"
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "pub functions should be exempt: {diags:?}"
    );
}

#[test]
fn test_function_passed_as_value() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn transformer(x) {
    return x * 2
}
let f = transformer
log(f(5))
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "function referenced as value should not trigger: {diags:?}"
    );
}

#[test]
fn test_function_called_from_another_function() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn inner() {
    return 42
}
fn outer() {
    return inner()
}
log(outer())
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "function called from another function should not trigger: {diags:?}"
    );
}

#[test]
fn test_pipeline_not_flagged_as_unused() {
    let diags = lint_source(
        r#"
pipeline default(task) {
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "pipelines should never trigger unused-function: {diags:?}"
    );
}

#[test]
fn test_impl_methods_exempt() {
    let diags = lint_source(
        r#"
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
let p = Point({x: 3, y: 4})
log(p)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "impl methods should be exempt: {diags:?}"
    );
}

#[test]
fn test_recursive_function_called_externally() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn factorial(n) {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}
log(factorial(5))
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "recursive function called externally should not trigger: {diags:?}"
    );
}

#[test]
fn test_mutually_recursive_functions_one_called() {
    let diags = lint_source(
        r#"
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
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "mutually recursive functions where one is called should not trigger: {diags:?}"
    );
}

#[test]
fn test_underscore_prefixed_function_exempt() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn _unused_helper() {
    return 42
}
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-function"),
        "underscore-prefixed functions should be exempt: {diags:?}"
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
        r#"
pipeline default(task) {
fn helper1() { return 1 }
fn helper2() { return 2 }
fn used() { return 3 }
log(used())
}
"#,
    );
    assert_eq!(
        count_rule(&diags, "unused-function"),
        2,
        "expected 2 unused-function warnings, got: {diags:?}"
    );
}

#[test]
fn test_top_level_unused_function() {
    let diags = lint_source(
        r#"
fn orphan() {
return 42
}
pipeline default(task) {
log("hello")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-function"),
        "top-level unused function should trigger: {diags:?}"
    );
}

#[test]
fn test_unused_function_with_wildcard_import() {
    // Wildcard imports shouldn't suppress unused-function checks —
    // external code can't call local non-pub functions.
    let diags = lint_source(
        r#"
import "some_module"
pipeline default(task) {
fn helper() { return 1 }
log("hello")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-function"),
        "unused-function should still fire with wildcard imports: {diags:?}"
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
