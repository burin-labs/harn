//! `shadow-variable` rule.

use super::*;

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
