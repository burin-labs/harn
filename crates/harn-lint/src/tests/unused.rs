//! `unused-variable` and `unused-parameter` coverage, plus their
//! autofix variants. The cross-rule `test_multiple_rules` test lives
//! here because its primary anchor is the unused-variable diagnostic.

use super::*;

#[test]
fn test_unused_variable() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const unused = 42
log("hello")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-variable"),
        "expected unused-variable warning, got: {diags:?}"
    );
}

#[test]
fn test_public_module_bindings_are_externally_reachable() {
    let diags = lint_source(
        r#"
pub const EXPORTED_SETTING: string = "configured"
pub let exported_counter = 0
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "public module bindings must not be treated as file-local dead code: {diags:?}"
    );
}

#[test]
fn test_private_module_and_local_bindings_remain_checked() {
    let diags = lint_source(
        r#"
const PRIVATE_SETTING = "configured"

fn read_setting() {
  const local_setting = "local"
  return PRIVATE_SETTING
}
"#,
    );
    assert!(
        diags.iter().any(|diagnostic| {
            diagnostic.rule == "unused-variable" && diagnostic.message.contains("`local_setting`")
        }),
        "unused local binding should still be reported: {diags:?}"
    );
    assert!(
        !diags.iter().any(|diagnostic| {
            diagnostic.rule == "unused-variable" && diagnostic.message.contains("`PRIVATE_SETTING`")
        }),
        "referenced private module binding should remain clean: {diags:?}"
    );

    let unused_private = lint_source("const PRIVATE_SETTING = \"configured\"");
    assert!(
        unused_private.iter().any(|diagnostic| {
            diagnostic.rule == "unused-variable" && diagnostic.message.contains("`PRIVATE_SETTING`")
        }),
        "unused private module binding should still be reported: {unused_private:?}"
    );

    let invalid_public_local = lint_source(
        r#"
fn invalid_export() {
  pub const local_setting = "local"
}
"#,
    );
    assert!(
        invalid_public_local.iter().any(|diagnostic| {
            diagnostic.rule == "unused-variable" && diagnostic.message.contains("`local_setting`")
        }),
        "a local pub modifier must not make the binding externally reachable: {invalid_public_local:?}"
    );
}

#[test]
fn test_unused_underscore_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const _ = 42
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "underscore variables should not trigger unused-variable: {diags:?}"
    );
}

#[test]
fn test_unused_underscore_prefixed_local_warns() {
    let source = r#"
pipeline default(task) {
const _cleanup = cleanup()
log("hello")
}
"#;
    let diags = lint_source(source);
    assert!(
        diags
            .iter()
            .any(|d| d.rule == "unused-variable" && d.message.contains("`_cleanup`")),
        "expected unused-variable for underscore-prefixed local, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const _ = cleanup()"),
        "expected underscore-prefixed local to autofix to discard binding, got: {result}"
    );
}

#[test]
fn test_used_underscore_prefixed_local_is_not_rewritten() {
    let source = r"
pipeline default(task) {
const _totals = record_usage()
log(_totals)
}
";
    let diags = lint_source(source);
    assert!(
        !diags
            .iter()
            .any(|d| d.rule == "unused-variable" && d.message.contains("`_totals`")),
        "used underscore-prefixed locals must not trigger unused-variable: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const _totals = record_usage()"),
        "used underscore-prefixed local must not be rewritten: {result}"
    );
}

#[test]
fn test_unused_underscore_prefixed_pattern_binding_ignored() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const { _ignored } = { _ignored: 42 }
log("hello")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-pattern-binding"),
        "underscore-prefixed pattern bindings should stay intent-preserving: {diags:?}"
    );
}

#[test]
fn test_unused_discard_parameter_ignored() {
    let diags = lint_source(
        r#"
pipeline default() {
fn greet(name, _) {
    log(name)
}
const f = { _, value -> log(value) }
greet("hi", "there")
f("ignored", "kept")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "discard parameters should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_unused_fn_param() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn greet(name, unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused fn param, got: {diags:?}"
    );
    // Should NOT trigger unused-variable (parameters are tracked separately).
    assert!(
        !has_rule(&diags, "unused-variable"),
        "unused fn param should not trigger unused-variable: {diags:?}"
    );
    let result = apply_fixes(
        "pipeline default(task) {\nfn greet(name, unused: HarnessTools) {\n    log(name)\n}\ngreet(\"hi\", {})\n}",
        &lint_source(
            "pipeline default(task) {\nfn greet(name, unused: HarnessTools) {\n    log(name)\n}\ngreet(\"hi\", {})\n}",
        ),
    );
    assert!(
        result.contains("fn greet(name, _unused: HarnessTools)"),
        "unused parameter repair must preserve positional arity: {result}"
    );
}

#[test]
fn unused_runtime_pipeline_slot_is_removed_instead_of_renamed() {
    let source = r#"pipeline default(harness: Harness, _task: unknown) {
  harness.stdio.println("ready")
}"#;
    let diagnostics = lint_source(source);
    assert!(
        has_rule(&diagnostics, "unused-pipeline-input"),
        "legacy underscore slots should participate in pipeline removal: {diagnostics:?}"
    );
    assert_eq!(
        apply_fixes(source, &diagnostics),
        r#"pipeline default(harness: Harness) {
  harness.stdio.println("ready")
}"#
    );
}

#[test]
fn pipeline_slot_removal_preserves_neighbors_and_their_comments() {
    let source = r"pipeline default(first: int, _unused: int, last: int) {
  log(first + last)
}";
    assert_eq!(
        apply_fixes(source, &lint_source(source)),
        r"pipeline default(first: int, last: int) {
  log(first + last)
}"
    );

    let documented_next = r"pipeline default(_unused: int, // belongs to value
  value: int) {
  log(value)
}";
    assert_eq!(
        apply_fixes(documented_next, &lint_source(documented_next)),
        r"pipeline default( // belongs to value
  value: int) {
  log(value)
}"
    );

    let all_unused = r"pipeline default(_first: int, _second: int) {
  return 1
}";
    assert_eq!(
        apply_fixes(all_unused, &lint_source(all_unused)),
        r"pipeline default() {
  return 1
}",
        "one fix pass must remove adjacent unused slots without overlap"
    );
}

#[test]
fn unused_bare_test_pipeline_slot_is_removed() {
    let source = r"@test
pipeline test_ready(_task: unknown) {
  assert(true)
}";
    assert_eq!(
        apply_fixes(source, &lint_source(source)),
        r"@test
pipeline test_ready() {
  assert(true)
}"
    );
}

#[test]
fn caller_and_table_bound_pipeline_slots_keep_positional_arity() {
    let called = r"pipeline helper(_value: int) {
  return 1
}

pipeline default() {
  return helper(2)
}";
    assert!(
        apply_fixes(called, &lint_source(called)).contains("pipeline helper(_value: int)"),
        "a local caller owns the helper's positional slot"
    );

    let table = r#"@test(cases: [{name: "one", args: [1]}])
pipeline test_case(value: int) {
  assert(true)
}"#;
    assert!(
        apply_fixes(table, &lint_source(table)).contains("pipeline test_case(_value: int)"),
        "table rows own the test pipeline's positional slots"
    );
}

#[test]
fn extended_pipeline_slots_keep_positional_arity() {
    let extended = r"pipeline child(value: int) extends base {
  return 1
}";
    assert!(
        apply_fixes(extended, &lint_source(extended)).contains("pipeline child(_value: int)"),
        "an extended pipeline inherits a positional contract"
    );
}

#[test]
fn public_pipeline_slots_keep_positional_arity() {
    let source = r"pub pipeline exported(value: int) {
  return 1
}";
    assert!(
        apply_fixes(source, &lint_source(source)).contains("pub pipeline exported(_value: int)"),
        "external callers may depend on a public pipeline's arity"
    );
}

#[test]
fn test_unused_closure_param() {
    let diags = lint_source(
        r"
pipeline default(task) {
const f = { x, y -> log(x) }
f(1, 2)
}
",
    );
    assert!(
        has_rule(&diags, "unused-parameter"),
        "expected unused-parameter for unused closure param, got: {diags:?}"
    );
}

#[test]
fn test_unused_param_underscore_prefix_ignored() {
    let diags = lint_source(
        r#"
pipeline default() {
fn greet(name, _unused) {
    log(name)
}
greet("hi", "there")
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "underscore-prefixed params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_used_fn_param_ok() {
    let diags = lint_source(
        r"
pipeline default() {
fn add(a, b) {
    return a + b
}
log(add(1, 2))
}
",
    );
    assert!(
        !has_rule(&diags, "unused-parameter"),
        "used params should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_parallel_options_mark_variables_used() {
    let diags = lint_source(
        r"
pipeline default(task) {
const concurrency = 2
const results = parallel each [1, 2] with { max_concurrent: concurrency } { n -> n }
log(results)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "parallel options should mark referenced variables used: {diags:?}"
    );
}

#[test]
fn test_destructuring_defaults_mark_referenced_variables_used() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const persona = "p"
const kind = "repair"
const downstream = "review"
const { step_name = "crystallized_${persona}_${kind}_${downstream}", function_name = step_name + "_step" } = {}
log(function_name)
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "destructuring defaults should mark referenced variables used: {diags:?}"
    );
}

#[test]
fn test_parameter_defaults_mark_referenced_parameters_used() {
    let diags = lint_source(
        r#"
pipeline default(task) {
fn child_path(root, child = path_join(root, "child")) {
    return child
}
log(child_path("/tmp"))
}
"#,
    );
    assert!(
        !diags
            .iter()
            .any(|d| d.rule == "unused-parameter" && d.message.contains("`root`")),
        "parameters used by default values should not trigger unused-parameter: {diags:?}"
    );
}

#[test]
fn test_mutex_key_marks_variable_used() {
    let diags = lint_source(
        r#"
pipeline default(task) {
const key = "tenant-a"
mutex(key) {
    log("locked")
}
}
"#,
    );
    assert!(
        !has_rule(&diags, "unused-variable"),
        "mutex key expressions should mark referenced variables used: {diags:?}"
    );
}

#[test]
fn test_attribute_arguments_mark_nested_identifiers_used() {
    let diags = lint_source(
        r#"const evidence_used = ["https://example.com/spec"]
const metadata_used = "fixture-a"
const genuinely_unused = ["https://example.com/unused"]
const id = "dict keys are not source references"

fn attribute_only_fallback() -> bool { return true }

@invariant
@deterministic
@archivist(evidence: evidence_used, confidence: 0.9, source_date: "2026-08-01", coverage_examples: [{id: metadata_used}], fallback: attribute_only_fallback, trigger: schedule("*/30 * * * *"), autonomy: act_with_approval)
pub fn inspect(_slice, _ctx, _repo) -> bool { return true }
"#,
    );
    let unused: Vec<_> = diags
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::LintUnusedVariable)
        .collect();
    assert_eq!(unused.len(), 2, "only genuine source references count");
    assert_eq!(
        unused
            .iter()
            .map(|diagnostic| diagnostic.span.line)
            .collect::<Vec<_>>(),
        vec![3, 4],
        "diagnostics must target the unused declaration and colliding dict key"
    );
    assert!(
        !diags
            .iter()
            .any(|diagnostic| diagnostic.code == Code::LintUndefinedFunction),
        "call-shaped attribute sentinels are metadata, not runtime calls"
    );
    assert!(
        !diags
            .iter()
            .any(|diagnostic| diagnostic.code == Code::LintUnusedFunction),
        "an attribute-only function reference must count as a real use"
    );
}

#[test]
fn test_multiple_rules() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let unused = 1
return 0
log("dead")
}
"#,
    );
    assert!(has_rule(&diags, "unused-variable"));
    assert!(has_rule(&diags, "mutable-never-reassigned"));
    assert!(has_rule(&diags, "dead-code-after-return"));
    assert_eq!(count_rule(&diags, "dead-code-after-return"), 1);
}

#[test]
fn test_fix_unused_variable_simple_let_binding() {
    let source = "pipeline default(task) {\n  const unused_thing = 42\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    assert!(has_rule(&diags, "unused-variable"));
    let fix = get_fix(&diags, "unused-variable");
    assert!(
        fix.is_some(),
        "expected autofix for simple const binding, got: {diags:?}"
    );
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const _ = 42"),
        "expected discard binding, got: {result}"
    );
    assert!(
        !result.contains("const unused_thing"),
        "original name should be replaced, got: {result}"
    );
}

#[test]
fn test_fix_unused_variable_simple_let_binding_with_type() {
    // Type annotation between the name and `=` must not confuse the scan.
    // We use `const` (not `let`) so the `mutable-never-reassigned` autofix
    // doesn't also fire and combine with this one.
    let source = "pipeline default(task) {\n  const leftover: int = 3\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable").expect("expected autofix");
    assert_eq!(fix.len(), 1, "expected single-edit fix");
    let edit = &fix[0];
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    let renamed = {
        let before = &source[..edit.span.start];
        let after = &source[edit.span.end..];
        format!("{before}{}{after}", edit.replacement)
    };
    assert!(
        renamed.contains("const _: int = 3"),
        "expected discard binding with type annotation, got: {renamed}"
    );
    assert!(
        !renamed.contains("const leftover:"),
        "original name should be replaced, got: {renamed}"
    );
}

#[test]
fn test_no_fix_for_unused_variable_in_dict_destructuring() {
    // Destructuring patterns are intentionally not autofixed today — the
    // rename would need a per-field span we do not currently track. The
    // diagnostic must still fire with a suggestion so the user can fix
    // manually.
    let source = "pipeline default(task) {\n  const { a, b } = { a: 1, b: 2 }\n  log(a)\n}";
    let diags = lint_source(source);
    let unused: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "unused-pattern-binding")
        .collect();
    assert!(
        unused.iter().any(|d| d.message.contains("`b`")),
        "expected unused-pattern-binding for `b`, got: {diags:?}"
    );
    for diag in &unused {
        if diag.message.contains("`b`") {
            assert!(
                diag.fix.is_none(),
                "destructuring unused-pattern-binding must not autofix, got: {:?}",
                diag.fix
            );
            assert!(
                diag.suggestion.is_some(),
                "destructuring unused-pattern-binding must keep its suggestion"
            );
        }
    }
}

#[test]
fn test_fix_unused_variable_is_word_boundary_safe() {
    // The variable name also appears in the RHS expression. The autofix
    // must only rewrite the binding occurrence, not the reference inside
    // the initializer, so the resulting source still parses.
    let source =
        "pipeline default(task) {\n  const threshold_ms = threshold_ms_default()\n  log(\"hi\")\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "unused-variable");
    assert!(fix.is_some(), "expected autofix, got: {diags:?}");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const _ = threshold_ms_default()"),
        "expected only the LHS binding renamed, got: {result}"
    );
}
