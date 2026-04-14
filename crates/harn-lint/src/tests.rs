use super::*;
use harn_lexer::Lexer;
use harn_parser::Parser;

fn lint_source(source: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    lint_with_source(&program, source)
}

fn has_rule(diagnostics: &[LintDiagnostic], rule: &str) -> bool {
    diagnostics.iter().any(|d| d.rule == rule)
}

fn count_rule(diagnostics: &[LintDiagnostic], rule: &str) -> usize {
    diagnostics.iter().filter(|d| d.rule == rule).count()
}

#[test]
fn test_clean_code() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
log(x)
}
"#,
    );
    // x is used, task is a pipeline param -- should be clean.
    assert!(
        !has_rule(&diags, "unused-variable"),
        "expected no unused-variable, got: {diags:?}"
    );
}

#[test]
fn test_public_function_requires_harndoc() {
    let diags = lint_source(
        r#"
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_assert_outside_test_pipeline_warns() {
    let diags = lint_source(
        r#"
pipeline default(task) {
assert(true)
}
"#,
    );
    assert!(
        has_rule(&diags, "assert-outside-test"),
        "expected assert-outside-test warning, got: {diags:?}"
    );
}

#[test]
fn test_assert_inside_test_pipeline_is_allowed() {
    let diags = lint_source(
        r#"
pipeline test(task) {
assert_eq(1 + 1, 2)
}
"#,
    );
    assert!(
        !has_rule(&diags, "assert-outside-test"),
        "asserts inside test pipelines should be allowed: {diags:?}"
    );
}

#[test]
fn test_require_inside_test_pipeline_warns() {
    let diags = lint_source(
        r#"
pipeline test_example(task) {
require 1 + 1 == 2, "math still works"
}
"#,
    );
    assert!(
        has_rule(&diags, "require-in-test"),
        "expected require-in-test warning, got: {diags:?}"
    );
}

#[test]
fn test_require_outside_test_pipeline_is_allowed() {
    let diags = lint_source(
        r#"
pipeline default(task) {
require task != nil, "task is required"
}
"#,
    );
    assert!(
        !has_rule(&diags, "require-in-test"),
        "require outside tests should be allowed: {diags:?}"
    );
}

#[test]
fn test_public_function_with_harndoc_is_clean() {
    let diags = lint_source(
        r#"
/** Explain the public API. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_public_function_with_multiline_harndoc_is_clean() {
    let diags = lint_source(
        r#"
/**
 * Explain the public API.
 * Across multiple lines.
 */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_legacy_triple_slash_above_pub_fn_fires() {
    let diags = lint_source(
        r#"
/// Old-style doc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "expected legacy-doc-comment, got: {diags:?}"
    );
    // And the autofix should produce a canonical /** */ block.
    let fix = diags
        .iter()
        .find(|d| d.rule == "legacy-doc-comment")
        .and_then(|d| d.fix.as_ref())
        .expect("legacy-doc-comment must carry an autofix");
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0].replacement.contains("/**") && fix[0].replacement.contains("*/"),
        "replacement should be a canonical /** */ block: {:?}",
        fix[0].replacement
    );
}

#[test]
fn test_plain_double_slash_adjacent_to_pub_fn_fires() {
    let diags = lint_source(
        r#"
// Doc-by-adjacency.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "expected legacy-doc-comment for // adjacent to def, got: {diags:?}"
    );
}

#[test]
fn test_plain_double_slash_with_blank_line_does_not_fire() {
    let diags = lint_source(
        r#"
// unrelated comment

pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "legacy-doc-comment"),
        "// with blank-line gap should not be treated as doc: {diags:?}"
    );
}

#[test]
fn test_existing_block_doc_does_not_fire_legacy() {
    let diags = lint_source(
        r#"
/** Already canonical. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "legacy-doc-comment"),
        "/** */ block should not trigger legacy rule: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "/** */ block should satisfy missing-harndoc: {diags:?}"
    );
}

#[test]
fn test_plain_comment_does_not_satisfy_harndoc() {
    let diags = lint_source(
        r#"
// Not HarnDoc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_private_function_does_not_require_harndoc() {
    let diags = lint_source(
        r#"
fn helper() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_unused_variable() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let unused = 42
log("hello")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-variable"),
        "expected unused-variable warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_underscore_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let _ = 42
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "underscore variables should not trigger unused-variable: {diags:?}"
    );
}

#[test]
fn test_unreachable_code() {
    let diags = lint_source(
        r#"
pipeline default(task) {
return 1
log("never reached")
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code warning, got: {diags:?}"
    );
}

#[test]
fn test_no_unreachable_when_return_is_last() {
    let diags = lint_source(
        r#"
pipeline default(task) {
log("hello")
return 1
}
"#,
    );
    assert!(
        !has_rule(&diags, "unreachable-code"),
        "return at end should not trigger unreachable-code: {diags:?}"
    );
}

#[test]
fn test_mutable_never_reassigned() {
    let diags = lint_source(
        r#"
pipeline default(task) {
var x = 1
log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "mutable-never-reassigned"),
        "expected mutable-never-reassigned warning, got: {diags:?}"
    );
}

#[test]
fn test_mutable_reassigned_ok() {
    let diags = lint_source(
        r#"
pipeline default(task) {
var x = 1
x = 2
log(x)
}
"#,
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "reassigned var should not trigger mutable-never-reassigned: {diags:?}"
    );
}

#[test]
fn test_empty_block_if() {
    let diags = lint_source(
        r#"
pipeline default(task) {
if true {
}
}
"#,
    );
    assert!(
        has_rule(&diags, "empty-block"),
        "expected empty-block warning for if, got: {diags:?}"
    );
}

#[test]
fn test_shadow_variable() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
if true {
    let x = 2
    log(x)
}
log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "shadow-variable"),
        "expected shadow-variable warning, got: {diags:?}"
    );
}

#[test]
fn test_no_shadow_same_scope() {
    // Re-declaration in the same scope is not shadowing (it may be a
    // parser error, but the linter only checks outer-scope shadows).
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
log(x)
}
"#,
    );
    assert!(
        !has_rule(&diags, "shadow-variable"),
        "same-scope should not trigger shadow-variable: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_throw() {
    let diags = lint_source("pipeline t(task) { throw \"err\"\nlog(\"unreachable\") }");
    assert!(
        diags.iter().any(|d| d.rule == "unreachable-code"),
        "expected unreachable-code after throw, got: {diags:?}"
    );
}

#[test]
fn test_unused_fn_param() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn greet(name, unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused fn param, got: {diags:?}"
    );
    // Should NOT trigger unused-variable (parameters are tracked separately).
    assert!(
        !has_rule(&diags, "unused-variable"),
        "unused fn param should not trigger unused-variable: {diags:?}"
    );
}

#[test]
fn test_unused_closure_param() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let f = { x, y -> log(x) }
f(1, 2)
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused closure param, got: {diags:?}"
    );
}

#[test]
fn test_unused_param_underscore_prefix_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn greet(name, _unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "underscore-prefixed params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_used_fn_param_ok() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn add(a, b) {
    return a + b
}
log(add(1, 2))
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "used params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_multiple_rules() {
    let diags = lint_source(
        r#"
pipeline default(task) {
var unused = 1
return 0
log("dead")
}
"#,
    );
    assert!(has_rule(&diags, "unused-variable"));
    assert!(has_rule(&diags, "mutable-never-reassigned"));
    assert!(has_rule(&diags, "unreachable-code"));
    assert_eq!(count_rule(&diags, "unreachable-code"), 1);
}

#[test]
fn test_comparison_to_bool_true() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = true
if x == true { log("yes") }
}
"#,
    );
    assert!(
        has_rule(&diags, "comparison-to-bool"),
        "expected comparison-to-bool, got: {diags:?}"
    );
}

#[test]
fn test_comparison_to_bool_false() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = true
if x == false { log("no") }
}
"#,
    );
    assert!(
        has_rule(&diags, "comparison-to-bool"),
        "expected comparison-to-bool, got: {diags:?}"
    );
}

#[test]
fn test_no_comparison_to_bool_for_normal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
if x == 1 { log("one") }
}
"#,
    );
    assert!(
        !has_rule(&diags, "comparison-to-bool"),
        "should not trigger for non-bool comparison: {diags:?}"
    );
}

#[test]
fn test_unnecessary_else_return() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
if x == 1 {
    return "one"
} else {
    return "other"
}
}
"#,
    );
    assert!(
        has_rule(&diags, "unnecessary-else-return"),
        "expected unnecessary-else-return, got: {diags:?}"
    );
}

#[test]
fn test_no_unnecessary_else_return_when_no_return() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
if x == 1 {
    log("one")
} else {
    log("other")
}
}
"#,
    );
    assert!(
        !has_rule(&diags, "unnecessary-else-return"),
        "should not trigger when branches don't return: {diags:?}"
    );
}

#[test]
fn test_duplicate_match_arm() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
match x {
    1 -> { log("one") }
    1 -> { log("also one") }
    _ -> { log("other") }
}
}
"#,
    );
    assert!(
        has_rule(&diags, "duplicate-match-arm"),
        "expected duplicate-match-arm, got: {diags:?}"
    );
}

#[test]
fn test_break_outside_loop() {
    let diags = lint_source(
        r#"
pipeline default(task) {
break
}
"#,
    );
    assert!(
        has_rule(&diags, "break-outside-loop"),
        "expected break-outside-loop, got: {diags:?}"
    );
}

#[test]
fn test_break_inside_loop_ok() {
    let diags = lint_source(
        r#"
pipeline default(task) {
while true {
    break
}
}
"#,
    );
    assert!(
        !has_rule(&diags, "break-outside-loop"),
        "break inside loop should be fine: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_break() {
    let diags = lint_source(
        r#"
pipeline default(task) {
while true {
    break
    log("unreachable")
}
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code after break, got: {diags:?}"
    );
}

#[test]
fn test_unreachable_after_composite_exit() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 } else { throw "err" }
    log("unreachable")
}
foo(true)
}
"#,
    );
    assert!(
        has_rule(&diags, "unreachable-code"),
        "expected unreachable-code after composite exit, got: {diags:?}"
    );
}

#[test]
fn test_no_unreachable_without_both_branches_exiting() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn foo(x: bool) {
    if x { return 1 }
    log("reachable")
}
foo(true)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unreachable-code"),
        "should not flag reachable code: {diags:?}"
    );
}

// ===== unused-function tests =====

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
fn test_collect_selective_import_names() {
    let source = r#"
import { foo, bar } from "module_a"
import { baz } from "module_b"
import "wildcard_module"
fn local() { return foo() + bar() + baz() }
"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();

    let names = collect_selective_import_names(&program);
    assert!(names.contains("foo"), "should contain foo");
    assert!(names.contains("bar"), "should contain bar");
    assert!(names.contains("baz"), "should contain baz");
    assert_eq!(names.len(), 3, "should have exactly 3 names: {names:?}");
}

// -----------------------------------------------------------------------
// Autofix tests
// -----------------------------------------------------------------------

/// Get the first fix for a given rule, or None.
fn get_fix(diagnostics: &[LintDiagnostic], rule: &str) -> Option<Vec<FixEdit>> {
    diagnostics
        .iter()
        .find(|d| d.rule == rule)
        .and_then(|d| d.fix.clone())
}

/// Apply all non-overlapping fixes to the source (reverse order).
fn apply_fixes(source: &str, diagnostics: &[LintDiagnostic]) -> String {
    let mut edits: Vec<&FixEdit> = diagnostics
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .flatten()
        .collect();
    edits.sort_by(|a, b| b.span.start.cmp(&a.span.start));
    let mut accepted: Vec<&FixEdit> = Vec::new();
    for edit in &edits {
        let overlaps = accepted
            .iter()
            .any(|prev| edit.span.start < prev.span.end && edit.span.end > prev.span.start);
        if !overlaps {
            accepted.push(edit);
        }
    }
    let mut result = source.to_string();
    for edit in &accepted {
        let before = &result[..edit.span.start];
        let after = &result[edit.span.end..];
        result = format!("{before}{}{after}", edit.replacement);
    }
    result
}

#[test]
fn test_fix_mutable_never_reassigned() {
    let source = "pipeline default(task) {\n  var x = 10\n  log(x)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "mutable-never-reassigned");
    assert!(fix.is_some(), "expected fix for mutable-never-reassigned");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let x = 10"),
        "expected var→let, got: {result}"
    );
    assert!(
        !result.contains("var x"),
        "var should be replaced, got: {result}"
    );
}

#[test]
fn test_fix_comparison_to_bool_true() {
    let source = "pipeline default(task) {\n  let x = true\n  let y = x == true\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let y = x"),
        "expected simplified comparison, got: {result}"
    );
    assert!(
        !result.contains("== true"),
        "should remove == true, got: {result}"
    );
}

#[test]
fn test_fix_comparison_to_bool_false() {
    let source = "pipeline default(task) {\n  let x = true\n  let y = x == false\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let y = !x"),
        "expected negated, got: {result}"
    );
}

#[test]
fn test_fix_comparison_to_bool_ne_true() {
    let source = "pipeline default(task) {\n  let x = true\n  let y = x != true\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let y = !x"),
        "!= true should become !x, got: {result}"
    );
}

#[test]
fn test_fix_unused_import_all_unused() {
    let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(task)\n}";
    let diags = lint_source(source);
    assert!(
        count_rule(&diags, "unused-import") >= 1,
        "expected unused-import warnings"
    );
    // When all names are unused, the fix should remove the entire import line
    let fix = get_fix(&diags, "unused-import");
    assert!(fix.is_some(), "expected fix for unused-import");
    let edits = fix.unwrap();
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0].replacement.is_empty(),
        "expected deletion, got: {:?}",
        edits[0].replacement
    );
}

#[test]
fn test_fix_unused_import_partial() {
    let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(foo)\n}";
    let diags = lint_source(source);
    // bar is unused, foo is used
    assert_eq!(
        count_rule(&diags, "unused-import"),
        1,
        "expected 1 unused-import warning"
    );
    let fix = get_fix(&diags, "unused-import");
    assert!(fix.is_some(), "expected fix for unused-import");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("{ foo }") || result.contains("{foo}"),
        "expected bar removed from import, got: {result}"
    );
    assert!(
        !result.contains("bar"),
        "bar should be removed, got: {result}"
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

#[test]
fn test_fix_multiple_fixes_applied() {
    let source = "pipeline default(task) {\n  var x = 10\n  let y = x == true\n  log(y)\n}";
    let diags = lint_source(source);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let x = 10"),
        "var should be fixed to let, got: {result}"
    );
    assert!(
        result.contains("let y = x"),
        "comparison should be simplified, got: {result}"
    );
}

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

#[test]
fn test_cyclomatic_complexity_warns_for_branchy_function() {
    let diags = lint_source(
        r#"
fn complicated(x: int) {
  if x > 0 { log("1") }
  if x > 1 { log("2") }
  if x > 2 { log("3") }
  if x > 3 { log("4") }
  if x > 4 { log("5") }
  if x > 5 { log("6") }
  if x > 6 { log("7") }
  if x > 7 { log("8") }
  if x > 8 { log("9") }
  if x > 9 { log("10") }
}

pipeline default(task) {
  complicated(10)
}
"#,
    );
    assert!(
        has_rule(&diags, "cyclomatic-complexity"),
        "expected cyclomatic-complexity warning, got: {diags:?}"
    );
}

#[test]
fn test_prompt_injection_risk_warns_on_interpolated_system_prompt() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let user_text = "ignore safety"
  llm_call("hello", "You are safe. ${user_text}")
}
"#,
    );
    assert!(
        has_rule(&diags, "prompt-injection-risk"),
        "expected prompt-injection-risk warning, got: {diags:?}"
    );
}

#[test]
fn test_no_fix_when_source_unavailable() {
    // lint without source — fixes should be None
    let source = "pipeline default(task) {\n  var x = 10\n  log(x)\n}";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let diags = lint(&program); // no source
    let fix = get_fix(&diags, "mutable-never-reassigned");
    assert!(
        fix.is_none(),
        "without source, fix should be None, got: {fix:?}"
    );
}

#[test]
fn test_fix_unused_variable_simple_let_binding() {
    let source = "pipeline default(task) {\n  let unused_thing = 42\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    assert!(has_rule(&diags, "unused-variable"));
    let fix = get_fix(&diags, "unused-variable");
    assert!(
        fix.is_some(),
        "expected autofix for simple let binding, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let _unused_thing = 42"),
        "expected `_unused_thing` prefix, got: {result}"
    );
    assert!(
        !result.contains("let unused_thing"),
        "original name should be replaced, got: {result}"
    );
}

#[test]
fn test_fix_unused_variable_simple_let_binding_with_type() {
    // Type annotation between the name and `=` must not confuse the scan.
    // We use `let` (not `var`) so the `mutable-never-reassigned` autofix
    // doesn't also fire and combine with this one.
    let source = "pipeline default(task) {\n  let leftover: int = 3\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable").expect("expected autofix");
    assert_eq!(fix.len(), 1, "expected single-edit fix");
    let edit = &fix[0];
    let renamed = {
        let before = &source[..edit.span.start];
        let after = &source[edit.span.end..];
        format!("{before}{}{after}", edit.replacement)
    };
    assert!(
        renamed.contains("let _leftover: int = 3"),
        "expected `_leftover: int` prefix, got: {renamed}"
    );
    assert!(
        !renamed.contains("let leftover:"),
        "original name should be replaced, got: {renamed}"
    );
}

#[test]
fn test_no_fix_for_unused_variable_in_dict_destructuring() {
    // Destructuring patterns are intentionally not autofixed today — the
    // rename would need a per-field span we do not currently track. The
    // diagnostic must still fire with a suggestion so the user can fix
    // manually.
    let source = "pipeline default(task) {\n  let { a, b } = { a: 1, b: 2 }\n  log(a)\n}";
    let diags = lint_source(source);
    let unused: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "unused-variable")
        .collect();
    assert!(
        unused.iter().any(|d| d.message.contains("`b`")),
        "expected unused-variable for `b`, got: {diags:?}"
    );
    for diag in &unused {
        if diag.message.contains("`b`") {
            assert!(
                diag.fix.is_none(),
                "destructuring unused-variable must not autofix, got: {:?}",
                diag.fix
            );
            assert!(
                diag.suggestion.is_some(),
                "destructuring unused-variable must keep its suggestion"
            );
        }
    }
}

#[test]
fn test_fix_empty_if_with_pure_condition() {
    let source = "pipeline default(task) {\n  let x = 3\n  if x > 0 { }\n  log(\"done\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "empty-block");
    assert!(
        fix.is_some(),
        "expected autofix for empty if with pure condition, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        !result.contains("if x > 0"),
        "empty if should be removed, got: {result}"
    );
    assert!(
        result.contains("log(\"done\")"),
        "sibling statement must survive, got: {result}"
    );
    // Re-parse so we know the rewrite produced valid source.
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_no_fix_for_empty_if_with_side_effecting_condition() {
    // side_effect() is a function call — removing the whole if would
    // drop the call, which could be observable. The diagnostic must
    // still fire but must not produce an autofix.
    let source = r#"
pipeline default(task) {
  if side_effect() { }
  log("hi")
}
"#;
    let diags = lint_source(source);
    let empty: Vec<_> = diags.iter().filter(|d| d.rule == "empty-block").collect();
    assert!(
        empty.iter().any(|d| d.message.contains("if")),
        "expected empty-block for if, got: {diags:?}"
    );
    assert!(
        empty.iter().all(|d| d.fix.is_none()),
        "side-effecting condition must not get an autofix"
    );
}

#[test]
fn test_no_fix_for_empty_if_with_else_branch() {
    // If both branches are empty we still warn, but autofixing the if
    // would silently drop the else body when someone fills it in later.
    // Conservative: no fix when else_body is present.
    let source = r#"
pipeline default(task) {
  let y = 1
  if y > 0 { } else { log("y") }
  log("end")
}
"#;
    let diags = lint_source(source);
    let empty: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "empty-block" && d.message.contains("if"))
        .collect();
    assert!(
        !empty.is_empty(),
        "expected empty-block for if, got: {diags:?}"
    );
    assert!(
        empty.iter().all(|d| d.fix.is_none()),
        "autofix must be suppressed when else branch exists"
    );
}

#[test]
fn test_fix_empty_for_with_pure_iterable() {
    let source = "pipeline default(task) {\n  let items = [1, 2, 3]\n  for item in items { }\n  log(\"done\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "empty-block");
    assert!(
        fix.is_some(),
        "expected autofix for empty for with pure iterable, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        !result.contains("for item in items"),
        "empty for should be removed, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_no_fix_for_empty_for_with_side_effecting_iterable() {
    let source = r#"
pipeline default(task) {
  for item in fetch_items() { }
  log("hi")
}
"#;
    let diags = lint_source(source);
    let empty: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "empty-block" && d.message.contains("for"))
        .collect();
    assert!(
        !empty.is_empty(),
        "expected empty-block for for-loop, got: {diags:?}"
    );
    assert!(
        empty.iter().all(|d| d.fix.is_none()),
        "side-effecting iterable must not get an autofix"
    );
}

#[test]
fn test_fix_redundant_nil_ternary_eq_pattern() {
    let source = r#"
pipeline default(task) {
  let x = 5
  let y = x == nil ? 0 : x
  log(y)
}
"#;
    let diags = lint_source(source);
    let fix = get_fix(&diags, "redundant-nil-ternary");
    assert!(
        fix.is_some(),
        "expected autofix for `x == nil ? 0 : x`, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let y = x ?? 0"),
        "expected `x ?? 0`, got: {result}"
    );
    let mut lexer = Lexer::new(&result);
    let tokens = lexer.tokenize().expect("relex after fix");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("reparse after fix");
}

#[test]
fn test_fix_redundant_nil_ternary_ne_pattern() {
    let source = r#"
pipeline default(task) {
  let x = 5
  let y = x != nil ? x : 0
  log(y)
}
"#;
    let diags = lint_source(source);
    let fix = get_fix(&diags, "redundant-nil-ternary");
    assert!(
        fix.is_some(),
        "expected autofix for `x != nil ? x : 0`, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let y = x ?? 0"),
        "expected `x ?? 0`, got: {result}"
    );
}

#[test]
fn test_no_warn_for_unrelated_ternary() {
    let source = r#"
pipeline default(task) {
  let a = 1
  let b = 2
  let c = a > b ? a : b
  log(c)
}
"#;
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "redundant-nil-ternary"),
        "unrelated ternary should not trigger redundant-nil-ternary, got: {diags:?}"
    );
}

#[test]
fn test_no_warn_when_non_nil_arm_differs_from_checked_var() {
    // `x != nil ? y : z` — the non-nil arm is NOT `x`, so the rewrite
    // would change semantics. Lint must stay silent.
    let source = r#"
pipeline default(task) {
  let x = 1
  let y = 2
  let z = 3
  let w = x != nil ? y : z
  log(w)
}
"#;
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "redundant-nil-ternary"),
        "rewrite would change semantics, lint must be silent, got: {diags:?}"
    );
}

#[test]
fn test_fix_unused_variable_is_word_boundary_safe() {
    // The variable name also appears in the RHS expression. The autofix
    // must only rewrite the binding occurrence, not the reference inside
    // the initializer, so the resulting source still parses.
    let source =
        "pipeline default(task) {\n  let threshold_ms = threshold_ms_default()\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable");
    assert!(fix.is_some(), "expected autofix, got: {diags:?}");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let _threshold_ms = threshold_ms_default()"),
        "expected only the LHS binding renamed, got: {result}"
    );
}

// ── untyped-dict-access lint rule ───────────────────────────────────

#[test]
fn test_untyped_dict_access_json_parse_property() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = json_parse(task).name
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "expected untyped-dict-access, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_yaml_parse_subscript() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = yaml_parse(task)["key"]
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "expected untyped-dict-access for yaml_parse subscript, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_not_flagged_on_dict_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = {"name": "test"}
    log(x.name)
}
"#,
    );
    assert!(
        !has_rule(&diags, "untyped-dict-access"),
        "dict literal access should not trigger rule, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_not_flagged_on_normal_function() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    fn get_data() -> dict {
        return {"name": "test"}
    }
    log(get_data().name)
}
"#,
    );
    assert!(
        !has_rule(&diags, "untyped-dict-access"),
        "non-boundary function should not trigger rule, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_llm_call() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = llm_call("p", "s").data
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "llm_call direct access should trigger rule, got: {diags:?}"
    );
}

// --- blank-line-between-items ---

#[test]
fn test_blank_line_between_items_fires_for_two_adjacent_fns() {
    let source = "fn a() -> int {\n  return 1\n}\nfn b() -> int {\n  return 2\n}\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "blank-line-between-items"),
        "expected blank-line-between-items, got: {diags:?}"
    );
}

#[test]
fn test_blank_line_between_items_ok_when_blank_present() {
    let source = "fn a() -> int {\n  return 1\n}\n\nfn b() -> int {\n  return 2\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "blank-line-between-items"),
        "should not fire with blank line present, got: {diags:?}"
    );
}

#[test]
fn test_blank_line_between_items_ok_with_doc_block_and_blank_above() {
    // Blank line above the doc block — doc block is glued to fn b.
    let source =
        "fn a() -> int {\n  return 1\n}\n\n/** Describes b. */\nfn b() -> int {\n  return 2\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "blank-line-between-items"),
        "blank line above doc block should satisfy the rule, got: {diags:?}"
    );
}

#[test]
fn test_blank_line_between_items_fires_when_doc_has_no_blank_above() {
    // No blank line above the doc block — the rule fires.
    let source =
        "fn a() -> int {\n  return 1\n}\n/** Describes b. */\nfn b() -> int {\n  return 2\n}\n";
    let diags = lint_source(source);
    let hit = diags
        .iter()
        .find(|d| d.rule == "blank-line-between-items")
        .expect("expected blank-line-between-items to fire");
    // Autofix should insert a newline at the start of the doc comment's
    // line (line 4) — the "\n" replacement lives above the doc block.
    let fix = hit.fix.as_ref().expect("autofix expected");
    assert_eq!(fix.len(), 1);
    assert_eq!(fix[0].replacement, "\n");
}

#[test]
fn test_blank_line_between_items_does_not_fire_between_imports() {
    let source = "import \"std/strings\"\nimport \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "blank-line-between-items"),
        "consecutive imports are intentionally tight, got: {diags:?}"
    );
}

// --- trailing-comma ---

#[test]
fn test_trailing_comma_fires_on_multiline_list() {
    let source =
        "pipeline default(task) {\n  let xs = [\n    1,\n    2,\n    3\n  ]\n  log(xs[0])\n}\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "trailing-comma"),
        "expected trailing-comma on multiline list, got: {diags:?}"
    );
}

#[test]
fn test_trailing_comma_ok_when_present() {
    let source =
        "pipeline default(task) {\n  let xs = [\n    1,\n    2,\n    3,\n  ]\n  log(xs[0])\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "trailing-comma"),
        "should not fire when comma already present, got: {diags:?}"
    );
}

#[test]
fn test_trailing_comma_ignores_single_line_list() {
    let source = "pipeline default(task) {\n  let xs = [1, 2, 3]\n  log(xs[0])\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "trailing-comma"),
        "single-line list should not fire, got: {diags:?}"
    );
}

#[test]
fn test_trailing_comma_ignores_fn_body_block() {
    // fn body is `{ ... }` but not a dict/struct — must not fire.
    let source = "fn x() -> int {\n  let y = 1\n  return y\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "trailing-comma"),
        "fn body block should not fire, got: {diags:?}"
    );
}

#[test]
fn test_trailing_comma_fires_on_dict_literal() {
    let source =
        "pipeline default(task) {\n  let d = {\n    \"a\": 1,\n    \"b\": 2\n  }\n  log(d)\n}\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "trailing-comma"),
        "expected trailing-comma on multiline dict, got: {diags:?}"
    );
}

#[test]
fn test_trailing_comma_fires_on_fn_call_args() {
    let source = "pipeline default(task) {\n  log(\n    \"first\",\n    \"second\"\n  )\n}\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "trailing-comma"),
        "expected trailing-comma on multiline call args, got: {diags:?}"
    );
}

// --- import-order ---

#[test]
fn test_import_order_fires_when_out_of_order() {
    let source = "import \"std/io\"\nimport \"std/fs\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "import-order"),
        "expected import-order when out of order, got: {diags:?}"
    );
}

#[test]
fn test_import_order_canonical_does_not_fire() {
    let source = "import \"std/fs\"\nimport \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "import-order"),
        "canonical order should not fire, got: {diags:?}"
    );
}

#[test]
fn test_import_order_single_import_does_not_fire() {
    let source = "import \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "import-order"),
        "single import should not fire, got: {diags:?}"
    );
}

#[test]
fn test_import_order_stdlib_before_third_party() {
    let source = "import \"mypkg/util\"\nimport \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "import-order"),
        "stdlib should come before third-party, got: {diags:?}"
    );
}

// --- require-file-header ---

fn lint_with_require_header(source: &str, path: Option<&std::path::Path>) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        file_path: path,
        require_file_header: true,
    };
    lint_with_options(
        &program,
        &[],
        Some(source),
        &std::collections::HashSet::new(),
        &options,
    )
}

#[test]
fn test_require_file_header_fires_when_missing() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "fn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    let hit = diags
        .iter()
        .find(|d| d.rule == "require-file-header")
        .expect("expected require-file-header");
    let fix = hit.fix.as_ref().expect("autofix expected");
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0].replacement.starts_with("/**\n * Foo."),
        "expected 'Foo.' title, got: {:?}",
        fix[0].replacement
    );
}

#[test]
fn test_require_file_header_ok_when_present() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "/**\n * Some header.\n */\nfn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    assert!(
        !has_rule(&diags, "require-file-header"),
        "should not fire when header present, got: {diags:?}"
    );
}

#[test]
fn test_require_file_header_fires_when_only_line_comment() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "// not a header\nfn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    assert!(
        has_rule(&diags, "require-file-header"),
        "// comment does not count as header, got: {diags:?}"
    );
}

#[test]
fn test_require_file_header_off_by_default() {
    // Without LintOptions { require_file_header: true }, the rule must
    // not fire — this protects the existing repo from sudden regressions.
    let diags = lint_source("fn main() -> int {\n  return 0\n}\n");
    assert!(
        !has_rule(&diags, "require-file-header"),
        "rule should be opt-in, got: {diags:?}"
    );
}

#[test]
fn test_derive_file_header_title_cases() {
    use std::path::PathBuf;
    let cases = [
        ("foo.harn", "Foo."),
        ("foo_bar.harn", "Foo bar."),
        ("foo-bar.harn", "Foo bar."),
        ("Foo.harn", "Foo."),
        ("data_pipeline.harn", "Data pipeline."),
        ("llm-cost.harn", "Llm cost."),
    ];
    for (name, expected) in cases {
        let path = PathBuf::from(name);
        let got = derive_file_header_title(Some(&path));
        assert_eq!(got, expected, "title for {name}");
    }
}

#[test]
fn test_derive_file_header_title_no_path_fallback() {
    let got = derive_file_header_title(None);
    assert_eq!(got, "Module.");
}
