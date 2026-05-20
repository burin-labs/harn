//! HARN-LNT-053 — ambient stdio builtins now route through
//! `harness.stdio.*`.

use super::*;

#[test]
fn ambient_stdio_call_inside_main_emits_lint_and_fixes_to_harness_stdio() {
    let source = "fn main(harness: Harness) {\n  println(\"hi\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-stdio-builtin"),
        1,
        "expected one ambient-stdio lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.stdio.println(\"hi\")"),
        "expected rewrite to harness.stdio.println(), got: {fixed}"
    );
    assert!(
        !fixed.contains(" println("),
        "ambient call should be gone, got: {fixed}"
    );
}

#[test]
fn ambient_stdio_lints_all_supported_names_inside_main() {
    let source = r#"fn main(harness: Harness) {
  print("a")
  println("b")
  eprint("c")
  eprintln("d")
  let line = read_line()
  let answer = prompt_user("q?")
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-stdio-builtin"),
        6,
        "expected one lint per ambient stdio call, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    for expected in [
        "harness.stdio.print",
        "harness.stdio.println",
        "harness.stdio.eprint",
        "harness.stdio.eprintln",
        "harness.stdio.read_line",
        "harness.stdio.prompt",
    ] {
        assert!(
            fixed.contains(expected),
            "expected {expected} in fixed source, got: {fixed}"
        );
    }
}

#[test]
fn ambient_stdio_lint_waits_until_harness_is_in_scope() {
    let source = "fn helper() {\n  println(\"hi\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-stdio-builtin"),
        1,
        "stdio migration lint should surface the threaded-harness path: {diags:?}"
    );
    let diag = diags
        .iter()
        .find(|diag| diag.rule == "ambient-stdio-builtin")
        .expect("stdio migration lint present");
    assert!(
        diag.fix.is_none(),
        "no direct fix should be emitted: {diag:?}"
    );
    let repair = diag.repair().expect("repair metadata present");
    assert_eq!(repair.id.as_str(), "bindings/thread-harness-needs-param");
    assert_eq!(repair.safety.as_str(), "surface-changing");
}
