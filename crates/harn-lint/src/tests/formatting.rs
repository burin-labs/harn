//! Surface-formatting rules: `blank-line-between-items`, `trailing-comma`,
//! and `eager-collection-conversion`.

use super::*;

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
fn test_eager_collection_conversion_let_list() {
    let source = r#"pipeline default(task) {
  let xs: list<int> = iter([1, 2, 3]).map(fn(x) { return x + 1 })
  log(xs)
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "eager-collection-conversion"),
        1,
        "expected exactly one eager-collection-conversion diagnostic, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains(".to_list()"),
        "expected autofix to append .to_list(), got: {fixed}"
    );
}

#[test]
fn test_eager_collection_conversion_no_flag_when_already_to_list() {
    let source = r#"pipeline default(task) {
  let xs: list<int> = iter([1, 2, 3]).map(fn(x) { return x + 1 }).to_list()
  log(xs)
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "eager-collection-conversion"),
        0,
        "should not flag already-materialized chains, got: {diags:?}"
    );
}

#[test]
fn test_eager_collection_conversion_return_stmt() {
    let source = r#"fn build() -> list<int> {
  return iter([1, 2, 3]).filter(fn(x) { return x > 0 })
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "eager-collection-conversion"),
        1,
        "expected eager-collection-conversion on return, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains(".to_list()"),
        "expected autofix to append .to_list(), got: {fixed}"
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

#[test]
fn test_eager_collection_conversion_ignores_iter_annotation() {
    let source = r#"pipeline default(task) {
  let xs: Iter<int> = iter([1, 2, 3]).map(fn(x) { return x + 1 })
  log(xs)
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "eager-collection-conversion"),
        0,
        "Iter<T> annotation should not trigger rule, got: {diags:?}"
    );
}
