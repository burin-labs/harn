//! Boolean-shape lint rules: `comparison-to-bool`,
//! `constant-logical-operand`, `unnecessary-else-return`,
//! `duplicate-match-arm`, plus autofix variants.

use super::*;

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
fn test_constant_logical_operand_autofix() {
    let source = "pipeline default(task) {\n  let x = true\n  let a = x || true\n  let b = x && false\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "constant-logical-operand"), 2);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let a = true") && result.contains("let b = false"),
        "expected constants, got: {result}"
    );
}

#[test]
fn test_constant_logical_operand_does_not_drop_impure_left_side() {
    let source = "pipeline default(task) {\n  let a = expensive() || true\n  let b = expensive() && false\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "constant-logical-operand"),
        0,
        "impure left sides must not be removed: {diags:?}"
    );
}

#[test]
fn test_leading_logical_constants_autofix_even_with_impure_right_side() {
    let source = "pipeline default(task) {\n  let a = true || expensive()\n  let b = false && expensive()\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "constant-logical-operand"), 2);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let a = true") && result.contains("let b = false"),
        "right sides are unreachable, got: {result}"
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
