//! `mutable-never-reassigned` — warning and `let → const` autofix.

use super::*;

#[test]
fn test_mutable_never_reassigned() {
    let diags = lint_source(
        r"
pipeline default(task) {
let x = 1
log(x)
}
",
    );
    assert!(
        has_rule(&diags, "mutable-never-reassigned"),
        "expected mutable-never-reassigned warning, got: {diags:?}"
    );
}

#[test]
fn test_mutable_reassigned_ok() {
    let diags = lint_source(
        r"
pipeline default(task) {
let x = 1
x = 2
log(x)
}
",
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "reassigned let should not trigger mutable-never-reassigned: {diags:?}"
    );
}

#[test]
fn test_fix_mutable_never_reassigned() {
    let source = "pipeline default(task) {\n  let x = 10\n  log(x)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "mutable-never-reassigned");
    assert!(fix.is_some(), "expected fix for mutable-never-reassigned");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const x = 10"),
        "expected let→const, got: {result}"
    );
    assert!(
        !result.contains("let x"),
        "let should be replaced, got: {result}"
    );
}
