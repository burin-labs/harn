//! Tests for the `prefer-optional-shorthand` rule: it must flag every
//! `T | nil` site (positionally precise), supply a fix that rewrites
//! to `T?`, and skip non-type contexts (comments, string literals).

use super::*;

#[test]
fn flags_let_binding_t_or_nil_with_fix() {
    let source = "pipeline default(task) {\n  const x: int | nil = nil\n  log(x)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 1);
    let fix = get_fix(&diags, "prefer-optional-shorthand").expect("fix is produced");
    assert_eq!(fix.len(), 1);
    assert_eq!(fix[0].replacement, "int?");
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("const x: int? = nil"),
        "expected sugared form in:\n{fixed}"
    );
}

#[test]
fn flags_each_t_or_nil_in_function_signature_separately() {
    let source = "pipeline default(task) {\n  fn first(text: string | nil, count: int | nil) -> string | nil {\n    return text\n  }\n  log(first(\"a\", 1))\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 3);
}

#[test]
fn skips_t_or_nil_inside_line_comment() {
    let source =
        "pipeline default(task) {\n  // legacy syntax: string | nil\n  const x: int = 0\n  log(x)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 0);
}

#[test]
fn skips_t_or_nil_inside_string_literal() {
    let source = "pipeline default(task) {\n  const s: string = \"string | nil\"\n  log(s)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 0);
}

#[test]
fn flags_real_annotation_when_string_literal_holds_decoy() {
    // When a let-binding has both a real `T | nil` annotation and a
    // value-side string containing the same text, the lint should pick
    // the type position, not the string content.
    let source =
        "pipeline default(task) {\n  const s: string | nil = \"string | nil decoy\"\n  log(s)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("const s: string? ="),
        "annotation should be sugared:\n{fixed}"
    );
    assert!(
        fixed.contains("\"string | nil decoy\""),
        "string content must remain untouched:\n{fixed}"
    );
}

#[test]
fn skips_when_arm_is_intersection_or_union() {
    // `(A & B) | nil` would have to print as `(A & B) | nil` to round-trip;
    // the sugar `A & B?` would re-bind `?` to the intersection's right
    // arm. The rule conservatively leaves that form alone.
    let source =
        "pipeline default(task) {\n  const x: {a: int} & {b: string} | nil = nil\n  log(x)\n}";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "prefer-optional-shorthand"), 0);
}
