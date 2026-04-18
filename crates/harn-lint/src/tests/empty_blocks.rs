//! `empty-block` warning plus conditional autofixes for `if` and `for`.

use super::*;

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
