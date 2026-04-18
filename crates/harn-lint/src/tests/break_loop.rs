//! `break-outside-loop` rule.

use super::*;

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
