//! HARN-LNT-052 — ambient clock builtins now route through
//! `harness.clock.*`. The lint owns the `bindings/thread-harness-clock`
//! repair that the E4.3 → E4.6 migration leans on.

use super::*;

#[test]
fn ambient_clock_call_inside_main_emits_lint_and_fixes_to_harness_clock() {
    let source =
        "fn main(harness: Harness) {\n  let ms = now_ms()\n  harness.stdio.println(ms)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-clock-builtin"),
        1,
        "expected one ambient-clock lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.clock.now_ms()"),
        "expected rewrite to harness.clock.now_ms(), got: {fixed}"
    );
    assert!(
        !fixed.contains(" now_ms("),
        "ambient call should be gone, got: {fixed}"
    );
}

#[test]
fn ambient_clock_lints_all_five_names_inside_main() {
    let source = r#"fn main(harness: Harness) {
  let a = now_ms()
  let b = monotonic_ms()
  let c = timestamp()
  let d = elapsed()
  sleep_ms(0)
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-clock-builtin"),
        5,
        "expected one lint per ambient call, got: {diags:?}"
    );
}

#[test]
fn ambient_clock_lint_without_harness_in_scope_emits_no_fix() {
    let source = "fn helper() {\n  let ms = now_ms()\n}\n";
    let diags = lint_source(source);
    let entries: Vec<&LintDiagnostic> = diags
        .iter()
        .filter(|d| d.rule == "ambient-clock-builtin")
        .collect();
    assert_eq!(entries.len(), 1, "expected one lint, got: {diags:?}");
    assert!(
        entries[0].fix.is_none(),
        "should not auto-fix without `harness` in scope, got: {:?}",
        entries[0].fix
    );
    assert!(
        entries[0]
            .suggestion
            .as_ref()
            .is_some_and(|s| s.contains("bindings/thread-harness")),
        "suggestion should point at the surface-changing repair, got: {:?}",
        entries[0].suggestion
    );
}

#[test]
fn underscore_harness_param_satisfies_in_scope_check() {
    let source = "fn main(_harness: Harness) {\n  let ms = now_ms()\n}\n";
    let diags = lint_source(source);
    let entry = diags
        .iter()
        .find(|d| d.rule == "ambient-clock-builtin")
        .expect("ambient-clock lint");
    assert!(
        entry.fix.is_some(),
        "_harness should count as in-scope for the lint's fix"
    );
}
