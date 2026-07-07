//! `unnecessary-parentheses` — removes grouping around one value when
//! the parens are not part of a call, declaration, or precedence boundary.

use super::*;

#[test]
fn test_unnecessary_parentheses_autofixes_ternary_list_branch() {
    let source = r#"pipeline default() {
  const args = repo ? (["--repo", repo]) : []
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "unnecessary-parentheses"),
        1,
        "expected unnecessary-parentheses, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains(r#"repo ? ["--repo", repo] : []"#),
        "expected autofix to unwrap list branch, got: {fixed}"
    );
}

#[test]
fn test_unnecessary_parentheses_ignores_keyed_mutex_parens() {
    // `mutex(resource)` parens are required grammar (the lock key), not
    // redundant grouping — stripping them would produce unparseable source.
    let source = r#"pipeline default(task) {
  mutex("a") {
    log(1)
  }
}
"#;
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "unnecessary-parentheses"),
        "keyed mutex parens must not be flagged, got: {diags:?}"
    );
}

#[test]
fn test_unnecessary_parentheses_autofixes_simple_return_value() {
    let source = r"fn repo_arg(repo) {
  return ( repo )
}
";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "unnecessary-parentheses"), 1);
    let fixed = apply_fixes(source, &diags);
    assert_eq!(fixed, "fn repo_arg(repo) {\n  return repo\n}\n");
}

#[test]
fn test_unnecessary_parentheses_ignores_call_and_decl_parens() {
    let source = r"fn repo_arg(repo) {
  log(repo)
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "unnecessary-parentheses"),
        "function declaration and call parens must not fire, got: {diags:?}"
    );
}

#[test]
fn test_unnecessary_parentheses_ignores_precedence_grouping() {
    let source = r"pipeline default() {
  const x = a * (b + c)
  const y = (a || b) && c
}
";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "unnecessary-parentheses"),
        "precedence-preserving parens must not fire, got: {diags:?}"
    );
}

#[test]
fn test_unnecessary_parentheses_only_unwraps_inner_nested_pair() {
    let source = r"pipeline default() {
  const repo = ((name))
}
";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "unnecessary-parentheses"),
        1,
        "only the inner parens should be fixed in one pass, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("(name)"),
        "expected one parenthesis layer to remain for the next lint pass, got: {fixed}"
    );
}
