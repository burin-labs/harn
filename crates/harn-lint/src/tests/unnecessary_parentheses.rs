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
fn test_unnecessary_parentheses_autofixes_wrapped_value_call() {
    let source = r"fn make(value) { return { other -> value + other } }
fn sum() { return (make(1)(2)) }
";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "unnecessary-parentheses"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("return make(1)(2)"),
        "expected autofix to unwrap value call, got: {fixed}",
    );
}

#[rstest::rstest]
// `mutex(resource)` parens are required grammar (the lock key), not
// redundant grouping — stripping them would produce unparseable source.
#[case::test_unnecessary_parentheses_ignores_keyed_mutex_parens(
    r#"pipeline default(task) {
  mutex("a") {
    log(1)
  }
}
"#,
    "unnecessary-parentheses",
    false
)]
#[case::test_unnecessary_parentheses_ignores_call_and_decl_parens(
    r"fn repo_arg(repo) {
  log(repo)
}
",
    "unnecessary-parentheses",
    false
)]
#[case::test_unnecessary_parentheses_ignores_precedence_grouping(
    r"pipeline default() {
  const x = a * (b + c)
  const y = (a || b) && c
}
",
    "unnecessary-parentheses",
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
