//! Boolean-shape lint rules: `comparison-to-bool`,
//! `constant-logical-operand`, `unnecessary-else-return`,
//! `duplicate-match-arm`, plus autofix variants.

use super::*;

#[test]
fn test_comparison_to_bool_true() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const x = true
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
const x = true
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
const x = 1
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
fn test_no_comparison_to_bool_for_optional_chaining() {
    // `d?.enabled == false` is a presence test: it is `false` when the
    // chain is nil, while the rewrite `!d?.enabled` would be `true`.
    // The rule (and its autofix) must not fire on optional-chained
    // operands, including chains continuing past the `?.` link.
    for expr in [
        "d?.enabled == false",
        "false == d?.enabled",
        "d?.flags.strict != true",
        "d?.[\"enabled\"] == false",
    ] {
        let diags = lint_source(&format!(
            "pipeline default(task) {{\n  const d = {{}}\n  if {expr} {{ log(\"x\") }}\n}}"
        ));
        assert!(
            !has_rule(&diags, "comparison-to-bool"),
            "should not trigger for `{expr}`: {diags:?}"
        );
    }
}

#[test]
fn optional_bool_comparisons_have_no_behavior_preserving_fix() {
    let source = r#"
fn optional(kind: string) -> bool? {
  if kind == "true" { return true }
  if kind == "false" { return false }
  return nil
}

fn main(harness: Harness) {
  for kind in ["true", "false", "nil"] {
    assert_eq(optional(kind) == true, kind == "true")
    assert_eq(optional(kind) == false, kind == "false")
    assert_eq(optional(kind) != false, kind != "false")
  }
  harness.stdio.println("pass")
}
"#;
    let diagnostics = lint_source(source);
    assert!(
        !has_rule(&diagnostics, "comparison-to-bool"),
        "optional comparisons are not redundant: {diagnostics:?}"
    );
    assert_eq!(execute_strict_source(source), "pass");
    let fixed = apply_fixes(source, &diagnostics);
    assert_eq!(
        fixed, source,
        "safe fixes must leave optional comparisons intact"
    );
    assert_eq!(execute_strict_source(&fixed), "pass");
    assert!(!has_rule(&lint_source(&fixed), "comparison-to-bool"));
}

#[test]
fn inferred_non_optional_bool_comparisons_remain_fixable() {
    let source = r#"
fn main(harness: Harness) {
  const yes = true
  const no = false
  assert(yes == true)
  assert(no == false)
  assert(yes != false)
  harness.stdio.println("pass")
}
"#;
    let diagnostics = lint_source(source);
    assert_eq!(
        count_rule(&diagnostics, "comparison-to-bool"),
        3,
        "all proven Boolean comparisons should remain fixable: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.rule == "comparison-to-bool")
            .all(|diagnostic| diagnostic.fix.is_some()),
        "every emitted comparison-to-bool diagnostic must carry its safe fix"
    );
    assert_eq!(execute_strict_source(source), "pass");
    assert_eq!(
        execute_strict_source(&apply_fixes(source, &diagnostics)),
        "pass"
    );
}

#[test]
fn test_constant_logical_operand_autofix() {
    let source = "pipeline default(task) {\n  const x = true\n  const a = x || true\n  const b = x && false\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "constant-logical-operand"), 2);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const a = true") && result.contains("const b = false"),
        "expected constants, got: {result}"
    );
}

#[test]
fn test_constant_logical_operand_does_not_drop_impure_left_side() {
    let source = "pipeline default(task) {\n  const a = expensive() || true\n  const b = expensive() && false\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "constant-logical-operand"),
        0,
        "impure left sides must not be removed: {diags:?}"
    );
}

#[test]
fn test_leading_logical_constants_autofix_even_with_impure_right_side() {
    let source = "pipeline default(task) {\n  const a = true || expensive()\n  const b = false && expensive()\n  log(a)\n  log(b)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "constant-logical-operand"), 2);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const a = true") && result.contains("const b = false"),
        "right sides are unreachable, got: {result}"
    );
}

#[test]
fn test_unnecessary_else_return() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const x = 1
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
const x = 1
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
const x = 1
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
    let source = "pipeline default(task) {\n  const x = true\n  const y = x == true\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const y = x"),
        "expected simplified comparison, got: {result}"
    );
    assert!(
        !result.contains("== true"),
        "should remove == true, got: {result}"
    );
}

#[test]
fn test_fix_comparison_to_bool_false() {
    let source = "pipeline default(task) {\n  const x = true\n  const y = x == false\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const y = !x"),
        "expected negated, got: {result}"
    );
}

#[test]
fn test_fix_comparison_to_bool_ne_true() {
    let source = "pipeline default(task) {\n  const x = true\n  const y = x != true\n  log(y)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "comparison-to-bool");
    assert!(fix.is_some(), "expected fix for comparison-to-bool");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const y = !x"),
        "!= true should become !x, got: {result}"
    );
}
