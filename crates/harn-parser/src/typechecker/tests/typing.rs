//! Basic typing: literals, fn / pipeline signatures, generics, type aliases, variance.

use super::*;
use crate::DiagnosticDetails;

#[test]
fn test_no_errors_for_untyped_code() {
    let errs = errors("pipeline t(task) { let x = 42\nlog(x) }");
    assert!(errs.is_empty());
}

#[test]
fn test_correct_typed_let() {
    let errs = errors("pipeline t(task) { let x: int = 42 }");
    assert!(errs.is_empty());
}

#[test]
fn test_type_mismatch_let() {
    let errs = errors(r#"pipeline t(task) { let x: int = "hello" }"#);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("expected int"));
    assert!(errs[0].contains("found string"));
}

#[test]
fn test_match_list_rest_binds_element_and_list_types() {
    // `[head, ...rest]` over `list<int>`: head: int, rest: list<int>.
    let errs = errors(
        r"pipeline t(task) {
  let xs: list<int> = [1, 2, 3]
  match xs {
    [head, ...rest] -> { let h: int = head
let r: list<int> = rest }
    _ -> { }
  }
}",
    );
    assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
}

#[test]
fn test_match_list_rest_type_is_refined_not_gradual() {
    // Assigning the rest binding to a non-list type must error — proving the
    // rest var is refined to a `list<…>` (not left untyped/gradual). Likewise
    // the leading binding is refined to the element type `int`.
    let errs = errors(
        r"pipeline t(task) {
  let xs: list<int> = [1, 2, 3]
  match xs {
    [head, ...rest] -> { let r: int = rest }
    _ -> { }
  }
}",
    );
    assert_eq!(errs.len(), 1, "expected one mismatch, got: {errs:?}");
    assert!(errs[0].contains("int"), "{errs:?}");

    let head_errs = errors(
        r"pipeline t(task) {
  let xs: list<int> = [1, 2, 3]
  match xs {
    [head, ...rest] -> { let h: string = head }
    _ -> { }
  }
}",
    );
    assert_eq!(
        head_errs.len(),
        1,
        "expected one mismatch, got: {head_errs:?}"
    );
}

#[test]
fn test_cyclic_type_aliases_do_not_recurse_forever() {
    let errs = errors(
        r"
type A = B
type B = A

pipeline t(task) {
  let x: A = 1
}
",
    );
    assert_eq!(errs.len(), 1, "expected one mismatch, got: {errs:?}");
    assert!(errs[0].contains("found int"), "{errs:?}");
}

#[test]
fn test_match_expression_infers_common_arm_type() {
    let errs = errors(
        r#"pipeline t(task) {
  let input = "b"
  let value: string = match input {
    "a" -> { "alpha" }
    "b" -> {
      let suffix = "ravo"
      "b" + suffix
    }
    _ -> { "other" }
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_match_expression_assignment_uses_arm_value_type() {
    let errs = errors(
        r#"pipeline t(task) {
  let input = "a"
  let value: int = match input {
    "a" -> { "alpha" }
    _ -> { "other" }
  }
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("expected int"));
    assert!(errs[0].contains("found string"));
}

#[test]
fn test_match_expression_mixed_arms_infer_union() {
    let errs = errors(
        r#"pipeline t(task) {
  let input = "a"
  let value: string | int = match input {
    "a" -> { "alpha" }
    _ -> { 42 }
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_match_expression_infers_list_pattern_binding_type() {
    let errs = errors(
        r"pipeline t(task) {
  let pair = [10, 20]
  let value: string = match pair {
    [_, item] -> { item }
    _ -> { 0 }
  }
}",
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("expected string"));
    assert!(errs[0].contains("found int"));
}

#[test]
fn test_correct_typed_fn() {
    let errs =
        errors("pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }\nadd(1, 2) }");
    assert!(errs.is_empty());
}

#[test]
fn test_rest_param_type_checks_each_argument() {
    let errs = errors(
        r#"pipeline t(task) {
  fn collect(...nums: int) -> list<int> { return nums }
  collect(1, "bad")
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("argument 2 `nums`"), "{errs:?}");
    assert!(errs[0].contains("expected int"), "{errs:?}");
    assert!(errs[0].contains("found string"), "{errs:?}");
}

#[test]
fn test_rest_param_binding_is_list_of_declared_type() {
    let errs = errors(
        r"pipeline t(task) {
  fn collect(...nums: int) {
    let values: list<int> = nums
  }
}",
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_transcript_append_builtins_preserve_input_container_type() {
    let errs = errors(
        r#"pipeline t(task) {
  var built = transcript({workflow: "demo"})
  built = add_user(built, [{type: "text", text: "hello", visibility: "public"}])
  built = add_assistant(built, [{type: "output_text", text: "done", visibility: "public"}])
  let messages = transcript_messages(built)
}"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_unnecessary_safe_navigation_warns_on_non_nil_receiver() {
    let diagnostics = check_source_with_source(
        r#"
type User = {name: string, email: string}
pipeline t(task) {
  let user: User = {name: "Ada", email: "ada@example.com"}
  log(user?.name)
  log(user?.email)
}
"#,
    );
    let safe_nav = diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.details,
                Some(DiagnosticDetails::LintRule { rule })
                    if *rule == "unnecessary-safe-navigation"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(safe_nav.len(), 2, "got diagnostics: {diagnostics:?}");
    assert!(
        safe_nav.iter().all(|diagnostic| {
            diagnostic
                .fix
                .as_ref()
                .is_some_and(|fix| fix.len() == 1 && fix[0].replacement == ".")
        }),
        "expected dot fixes: {safe_nav:?}"
    );
}

#[test]
fn test_unnecessary_safe_navigation_respects_nullable_and_unsupported_property() {
    let diagnostics = check_source_with_source(
        r"
type User = {name: string}
pipeline t(task) {
  let maybe: User? = nil
  log(maybe?.name)
  let n: int = 42
  log(n?.missing)
  let broad: dict = {}
  log(broad?.dynamic_field)
  let union_value: dict | list = broad
  log(union_value?.dynamic_field)
}
",
    );
    assert!(
        diagnostics.iter().all(|diagnostic| !matches!(
            &diagnostic.details,
            Some(DiagnosticDetails::LintRule { rule })
                if *rule == "unnecessary-safe-navigation"
        )),
        "nullable receivers and unsupported optional property access must not warn: {diagnostics:?}"
    );
}

#[test]
fn test_unnecessary_safe_navigation_uses_flow_narrowing_and_handles_postfix_forms() {
    let diagnostics = check_source_with_source(
        r#"
type User = {name: string}
pipeline t(task) {
  let maybe: User? = {name: "Ada"}
  if maybe != nil {
    log(maybe?.name)
  }
  let names: list<string> = ["Ada"]
  log(names?[0])
  log("Ada"?.lowercase())
}
"#,
    );
    let fixes = diagnostics
        .iter()
        .filter_map(|diagnostic| match &diagnostic.details {
            Some(DiagnosticDetails::LintRule { rule })
                if *rule == "unnecessary-safe-navigation" =>
            {
                diagnostic.fix.as_ref()
            }
            _ => None,
        })
        .flatten()
        .map(|fix| fix.replacement.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        fixes,
        vec![".", "", "."],
        "got diagnostics: {diagnostics:?}"
    );
}

#[test]
fn test_optional_access_on_dynamic_dict_union_stays_unknown() {
    let errs = errors(
        r"
pipeline t(task) {
  fn needs_string(target: string) {}
  let worker_summary: dict | list = {}
  needs_string(worker_summary?.snapshot_path)
}
",
    );
    assert!(
        errs.is_empty(),
        "dynamic dict access should not collapse to nil: {errs:?}"
    );
}

#[test]
fn test_optional_access_infers_nil_when_receiver_is_nullable() {
    let errs = errors(
        r#"
type User = {name: string}
pipeline t(task) {
  let maybe: User? = nil
  let name: string? = maybe?.name
  let lowered: string? = maybe?.name?.lowercase()
  let contains_a: bool? = maybe?.name?.contains("a")
}
"#,
    );
    assert!(errs.is_empty(), "unexpected errors: {errs:?}");
}

#[test]
fn test_flow_predicate_mode_attributes_are_recognized_on_functions() {
    let warns = warnings(
        r"
@deterministic
fn pure_check(slice) -> bool { return true }

@semantic
fn semantic_check(slice) -> bool { return true }
",
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")),
        "predicate mode attributes should not warn as unknown: {warns:?}"
    );
}

#[test]
fn test_runtime_attributes_are_recognized_on_valid_declarations() {
    let warns = warnings(
        r#"
@test
pipeline smoke(task) {}

@acp_skill(name: "deploy", when_to_use: "ship", invocation: "explicit")
fn deploy_activate() -> string { return "ready" }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")),
        "runtime attributes should not warn on valid declarations: {warns:?}"
    );
}

#[test]
fn test_test_scheduler_attributes_are_recognized_and_validated() {
    let warns = warnings(
        r#"
@test
@serial(group: "shared-fixture")
pipeline test_login_first(task) {}

@test
@heavy(threads: 2)
pipeline test_full_rebuild(task) {}

@test
@serial
pipeline test_bare_serial(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")
                && !warning.contains("@serial")
                && !warning.contains("@heavy")),
        "test scheduler attributes should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_job_retry_dict_and_standalone_retry_validate_identically() {
    // The compact `@job(retry: {...})` dict and the standalone `@retry(...)`
    // attribute are documented aliases and now share one validator, so they
    // MUST accept/reject the same backoff strategies. A valid strategy warns
    // on neither; an invalid one warns on both. Guards against the two
    // surfaces drifting (e.g. one list keeping a `"exp"` the other dropped).
    let valid = warnings(
        r#"
@job("nightly", retry: {max: 3, backoff: "exponential"})
@retry(max: 3, backoff: "linear")
fn nightly() -> string { return "ok" }
"#,
    );
    assert!(
        valid.iter().all(|w| !w.contains("backoff")),
        "recognized backoff strategies must warn on neither retry surface: {valid:?}"
    );

    let invalid = warnings(
        r#"
@job("nightly", retry: {max: 3, backoff: "exp"})
@retry(max: 3, backoff: "exp")
fn nightly() -> string { return "ok" }
"#,
    );
    let backoff_warns = invalid.iter().filter(|w| w.contains("backoff")).count();
    assert_eq!(
        backoff_warns, 2,
        "an unrecognized backoff must warn on BOTH retry surfaces (compact + standalone): {invalid:?}"
    );
}

#[test]
fn test_heavy_attribute_requires_positive_int_threads() {
    let warns = warnings(
        r#"
@test
@heavy
pipeline test_missing_threads(task) {}

@test
@heavy(threads: 0)
pipeline test_zero_threads(task) {}

@test
@heavy(threads: "lots")
pipeline test_string_threads(task) {}
"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("must specify `threads:")),
        "expected missing-threads warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .filter(|w| w.contains("must be a positive integer"))
            .count()
            == 2,
        "expected two positive-int warnings (for 0 and \"lots\"), got {warns:?}"
    );
}

#[test]
fn test_serial_heavy_attributes_warn_on_non_pipeline_targets() {
    let warns = warnings(
        r#"
@serial(group: "fixture")
fn helper(x) -> int { return x }

@heavy(threads: 2)
fn other_helper() -> int { return 0 }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@serial` only applies to pipeline declarations")),
        "expected @serial target warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@heavy` only applies to pipeline declarations")),
        "expected @heavy target warning, got {warns:?}"
    );
}

#[test]
fn test_durable_persona_annotations_are_recognized_and_validated() {
    let warns = warnings(
        r#"
@persona(
  triggers: [github.pr_opened, schedule("*/30 * * * *")],
  tools: [github, ci, linear],
  autonomy: act_with_approval,
  budget: {daily_usd: 20, frontier_escalations: 3},
  handoffs: [review_captain, human_maintainer],
  receipts: required,
)
@trigger(github.check_failed)
@handoff(target: review_captain, reason: "risky diff")
@budget(daily_usd: 20, max_tokens: 100000)
fn merge_captain(ctx) -> string { return "ok" }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")
                && !warning.contains("only applies")
                && !warning.contains("must")),
        "durable persona annotations should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_durable_persona_annotation_arg_type_warnings() {
    let warns = warnings(
        r#"
@persona(triggers: "github.pr_opened", tools: [github, 1], budget: {daily_usd: "twenty"})
@budget(max_tokens: "many")
fn bad_persona(ctx) { return ctx }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(triggers: ...)` must be a list")),
        "expected persona trigger list warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(tools: ...)` must contain only")),
        "expected persona tools warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@persona(daily_usd: ...)` must be a number")),
        "expected inline budget warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@budget(max_tokens: ...)` must be a number")),
        "expected budget warning, got {warns:?}"
    );
}

#[test]
fn test_command_attribute_recognized_on_pipelines_with_known_args() {
    let warns = warnings(
        r#"
@command(name: "review", description: "Review the diff", hint: "focus area")
pipeline review_branch(task) {}
"#,
    );
    assert!(
        warns.iter().all(|warning| !warning.contains("unknown")
            && !warning.contains("only applies")
            && !warning.contains("must")),
        "@command on a pipeline with known args should validate cleanly: {warns:?}"
    );
}

#[test]
fn test_command_attribute_warns_on_unknown_args() {
    let warns = warnings(
        r#"
@command(label: "oops")
pipeline review_branch(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unknown `@command` argument `label`")),
        "expected unknown-arg warning, got {warns:?}"
    );
}

#[test]
fn test_policy_attribute_is_recognized_and_validates_args() {
    // A well-formed `@policy(kinds: ...)` is recognized (no unknown-attr
    // warning) and clean.
    let clean = warnings(
        r#"
@policy(kinds: "operator platform_admin")
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        clean
            .iter()
            .all(|w| !w.contains("unknown attribute") && !w.contains("@policy")),
        "well-formed @policy should not warn: {clean:?}"
    );

    // An unknown key warns but the attribute is still recognized.
    let bad_key = warnings(
        r#"
@policy(roles: "operator")
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        bad_key
            .iter()
            .any(|w| w.contains("unknown `@policy` argument `roles`")),
        "expected unknown-arg warning, got {bad_key:?}"
    );

    // A non-string value warns.
    let bad_value = warnings(
        r#"
@policy(kinds: 42)
@route("POST", "/admin/x")
fn admin_x(req) { return req }
"#,
    );
    assert!(
        bad_value
            .iter()
            .any(|w| w.contains("`@policy(kinds: ...)` must be a string literal")),
        "expected non-string warning, got {bad_value:?}"
    );
}

#[test]
fn test_command_attribute_warns_on_function_decls() {
    let warns = warnings(
        r#"
@command(name: "review")
fn review_branch(task) {}
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("`@command` only applies to pipeline declarations")),
        "expected placement warning, got {warns:?}"
    );
}

#[test]
fn test_flow_predicate_mode_attributes_warn_off_functions() {
    let warns = warnings(
        r"
@deterministic
pipeline invalid(task) {}
",
    );
    assert!(
        warns
            .iter()
            .any(|warning| warning.contains("`@deterministic` only applies to function")),
        "expected placement warning, got {warns:?}"
    );
}

#[test]
fn test_flow_invariant_archivist_attributes_recognized() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["https://example.com/spec"], confidence: 0.95, source_date: "2026-04-01", coverage_examples: ["case-a"])
@retroactive
fn complete_predicate(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|warning| !warning.contains("unknown attribute")),
        "archivist/retroactive attributes should be recognised: {warns:?}"
    );
}

#[test]
fn test_flow_invariant_requires_kind_and_archivist() {
    let warns = warnings(
        r"
@invariant
fn bare_predicate(slice) -> bool { return true }
",
    );
    assert!(
        warns.iter().any(|w| w.contains("requires exactly one of")),
        "expected kind-required warning, got {warns:?}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("missing `@archivist(...)`")),
        "expected archivist-required warning, got {warns:?}"
    );
}

#[test]
fn test_flow_invariant_with_kind_only_still_warns_about_archivist() {
    let warns = warnings(
        r"
@invariant
@deterministic
fn kinded_predicate(slice) -> bool { return true }
",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("missing `@archivist(...)`")),
        "expected archivist-required warning, got {warns:?}"
    );
    assert!(
        warns.iter().all(|w| !w.contains("requires exactly one of")),
        "should not also warn about missing kind: {warns:?}"
    );
}

#[test]
fn test_flow_invariant_kinds_are_mutually_exclusive() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@semantic
@archivist(evidence: ["x"])
fn confused(slice) -> bool { return true }
"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("mutually exclusive")),
        "expected mutual-exclusion warning, got {warns:?}"
    );
}

#[test]
fn test_archivist_without_invariant_warns() {
    let warns = warnings(
        r#"
@archivist(evidence: ["https://x"])
fn standalone() -> int { return 1 }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("only applies to Flow predicates marked")),
        "expected standalone-archivist warning, got {warns:?}"
    );
}

#[test]
fn test_handler_ir_invariant_does_not_trigger_flow_lints() {
    // `@invariant("name")` is the harn-ir handler form, validated
    // separately. Flow lints must not fire for it.
    let warns = warnings(
        r#"
@invariant("approval.reachability")
fn handler() -> int { return 1 }
"#,
    );
    assert!(
        warns
            .iter()
            .all(|w| !w.contains("`@archivist(...)`") && !w.contains("requires exactly one of")),
        "handler-IR @invariant should not trigger Flow lints: {warns:?}"
    );
}

#[test]
fn test_archivist_unknown_arg_warns() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["x"], typo_key: "oops")
fn oops(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unknown `@archivist` argument `typo_key`")),
        "expected unknown-arg warning, got {warns:?}"
    );
}

#[test]
fn test_archivist_confidence_out_of_range_warns() {
    let warns = warnings(
        r#"
@invariant
@deterministic
@archivist(evidence: ["x"], confidence: 1.5)
fn loud(slice) -> bool { return true }
"#,
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("confidence") && w.contains("[0.0, 1.0]")),
        "expected confidence-range warning, got {warns:?}"
    );
}

#[test]
fn test_fn_arg_type_mismatch() {
    let errs = errors(
        r#"pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }
add("hello", 2) }"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("argument 1 `a`"));
    assert!(errs[0].contains("expected int"));
}

#[test]
fn test_option_bag_literal_rejects_unknown_field() {
    let errs = errors(
        r"type PickOptions = {drop_nil?: bool}

fn pick(options: PickOptions = {}) -> nil {
  return nil
}

pipeline t(task) {
  pick({dropnil: true})
}",
    );
    assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
    assert!(
        errs[0].contains("argument 1 `options`: unknown option `dropnil`"),
        "unexpected error: {}",
        errs[0]
    );
    assert!(
        errs[0].contains("did you mean `drop_nil`"),
        "missing suggestion: {}",
        errs[0]
    );
}

#[test]
fn test_non_option_shape_call_keeps_width_subtyping() {
    let errs = errors(
        r#"fn greet(u: {name: string}) -> string {
  return "hi " + u.name
}

pipeline t(task) {
  greet({name: "Bob", age: 25})
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_return_type_mismatch() {
    let errs = errors(r#"pipeline t(task) { fn get() -> int { return "hello" } }"#);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("return value: expected int, found string"));
}

#[test]
fn test_union_type_compatible() {
    let errs = errors(r"pipeline t(task) { let x: string | nil = nil }");
    assert!(errs.is_empty());
}

#[test]
fn test_union_type_mismatch() {
    let errs = errors(r"pipeline t(task) { let x: string | nil = 42 }");
    assert_eq!(errs.len(), 1);
    // Type-checker errors print the canonical sugared form for
    // `T | nil` unions; the source can use either spelling.
    assert!(
        errs[0].contains("expected string?"),
        "expected sugared form in: {}",
        errs[0]
    );
    assert!(errs[0].contains("found int"));
}

#[test]
fn test_var_nil_widens_on_first_concrete_assignment() {
    let errs = errors(
        r#"pipeline t(task) {
  var hit = nil
  hit = {name: "b", score: 2}
  let widened: {name: string, score: int} | nil = hit
  hit = nil
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_var_nil_widens_inside_nil_guard() {
    let errs = errors(
        r#"pipeline t(task) {
  var hit = nil
  if hit == nil {
    hit = {name: "b", score: 2}
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_explicit_nullable_var_annotation_still_accepts_nil_and_concrete() {
    let errs = errors(
        r#"pipeline t(task) {
  var hit: {name: string, score: int} | nil = nil
  hit = {name: "b", score: 2}
  hit = nil
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_explicit_nil_var_does_not_widen() {
    let errs = errors(
        r#"pipeline t(task) {
  var hit: nil = nil
  hit = {name: "b", score: 2}
}"#,
    );
    assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
    assert!(errs[0].contains("expected nil"), "got: {}", errs[0]);
}

#[test]
fn test_type_inference_propagation() {
    let errs = errors(
        r"pipeline t(task) {
  fn add(a: int, b: int) -> int { return a + b }
  let result: string = add(1, 2)
}",
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("expected string"));
    assert!(errs[0].contains("found int"));
    assert!(errs[0].contains("string"));
    assert!(errs[0].contains("int"));
}

#[test]
fn test_generic_return_type_instantiates_from_callsite() {
    let errs = errors(
        r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  fn first<T>(items: list<T>) -> T { return items[0] }
  let n: int = identity(42)
  let s: string = first(["a", "b"])
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_explicit_generic_call_type_args_are_checked() {
    let errs = errors(
        r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  let n: int = identity<int>(42)
  let words: [string] = identity<[string]>(["a", "b"])
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_explicit_generic_call_type_args_must_match_arguments() {
    let errs = errors(
        r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  let n: int = identity<int>("oops")
}"#,
    );
    assert_eq!(errs.len(), 2, "expected 2 errors, got: {errs:?}");
    assert!(
        errs.iter()
            .any(|err| err.contains("type parameter 'T' was inferred as both int and string")),
        "missing explicit type binding conflict error: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected int, found string")),
        "missing explicit type-arg mismatch error: {errs:?}"
    );
}

#[test]
fn test_generic_type_param_must_bind_consistently() {
    let errs = errors(
        r#"pipeline t(task) {
  fn keep<T>(a: T, b: T) -> T { return a }
  keep(1, "x")
}"#,
    );
    assert_eq!(errs.len(), 2, "expected 2 errors, got: {errs:?}");
    assert!(
        errs.iter()
            .any(|err| err.contains("type parameter 'T' was inferred as both int and string")),
        "missing generic binding conflict error: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("argument 2 `b`: expected int, found string")),
        "missing instantiated argument mismatch error: {errs:?}"
    );
}

#[test]
fn test_generic_list_binding_propagates_element_type() {
    let errs = errors(
        r"pipeline t(task) {
  fn first<T>(items: list<T>) -> T { return items[0] }
  let bad: string = first([1, 2, 3])
}",
    );
    assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
    assert!(errs[0].contains("expected string, found int"));
}

#[test]
fn test_generic_struct_literal_instantiates_type_arguments() {
    let errs = errors(
        r#"pipeline t(task) {
  struct Pair<A, B> {
first: A
second: B
  }
  let pair: Pair<int, string> = Pair { first: 1, second: "two" }
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_attributed_struct_forward_reference_is_registered() {
    let errs = errors(
        r#"pipeline t(task) {
  let point = Point { x: 1, y: 2 }
  @note("shape")
  struct Point {
    x: int
    y: int
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_unknown_struct_literal_reports_error() {
    let diagnostics = check_source(
        r"pipeline t(task) {
  let p = Point {x: 3, y: 4}
}",
    );
    let errors: Vec<_> = diagnostics
        .into_iter()
        .filter(|diag| diag.severity == DiagnosticSeverity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(errors[0].message, "unknown struct type `Point`");
}

#[test]
fn test_unknown_struct_literal_suggests_close_match() {
    let diagnostics = check_source(
        r"pipeline t(task) {
  struct Point {
    x: int
    y: int
  }

  let p = Piont {x: 3, y: 4}
}",
    );
    let errors: Vec<_> = diagnostics
        .into_iter()
        .filter(|diag| diag.severity == DiagnosticSeverity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "expected one error, got: {errors:?}");
    assert_eq!(
        errors[0].message,
        "unknown struct type `Piont` — did you mean `Point`?"
    );
    assert_eq!(
        errors[0].help.as_deref(),
        Some("declare `struct Point { ... }` or fix the type name")
    );
}

#[test]
fn test_generic_enum_construct_instantiates_type_arguments() {
    let errs = errors(
        r"pipeline t(task) {
  enum Option<T> {
Some(value: T),
None
  }
  let value: Option<int> = Option.Some(42)
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_result_generic_type_compatibility() {
    let errs = errors(
        r#"pipeline t(task) {
  let ok: Result<int, string> = Result.Ok(42)
  let err: Result<int, string> = Result.Err("oops")
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_result_generic_type_mismatch_reports_error() {
    let errs = errors(
        r"pipeline t(task) {
  let bad: Result<int, string> = Result.Err(42)
}",
    );
    assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
    assert!(errs[0].contains("Result<int, string>"));
    assert!(errs[0].contains("Result<_, int>"));
}

#[test]
fn test_builtin_return_type_inference() {
    let errs = errors(r#"pipeline t(task) { let x: string = to_int("42") }"#);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("string"));
    assert!(errs[0].contains("int"));
}

#[test]
fn test_harness_crypto_sha256_type_inference() {
    let errs = errors(
        r#"fn main(harness: Harness) {
  let crypto: HarnessCrypto = harness.crypto
  let digest: string = crypto.sha256("")
  let wrong: int = harness.crypto.sha256("hello")
}"#,
    );
    assert_eq!(errs.len(), 1, "expected one mismatch, got: {errs:?}");
    assert!(errs[0].contains("expected int"), "{errs:?}");
    assert!(errs[0].contains("found string"), "{errs:?}");
}

#[test]
fn test_builtin_arg_type_mismatch() {
    let errs = errors(r"pipeline t(task) { len(42) }");
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("argument 1 `value`"));
    assert!(errs[0].contains("expected"));
    assert!(errs[0].contains("found int"));
}

#[test]
fn test_harness_fs_method_return_type_inference() {
    let errs = errors(
        r#"fn main(harness: Harness) {
  let dir: string = harness.fs.mkdtemp("harn-type-")
  let matches: list = harness.fs.glob("*.toml", dir)
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_harness_fs_method_arg_type_mismatch() {
    let errs = errors(
        r"fn main(harness: Harness) {
  harness.fs.mkdtemp(42)
}",
    );
    assert_eq!(errs.len(), 1, "expected one error, got: {errs:?}");
    assert!(errs[0].contains("argument 1 `prefix`"), "{errs:?}");
    assert!(errs[0].contains("found int"), "{errs:?}");
}

#[test]
fn test_harness_llm_method_return_type_inference() {
    let errs = errors(
        r"fn main(harness: Harness) {
  let catalog: list = harness.llm.catalog()
  let refresh: dict = harness.llm.catalog_refresh({force: true})
  let providers: list = harness.llm.providers()
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_harness_llm_method_arg_type_mismatch() {
    let warns = warnings(
        r#"fn main(harness: Harness) {
  harness.llm.catalog("extra")
}"#,
    );
    assert_eq!(warns.len(), 1, "expected one warning, got: {warns:?}");
    assert!(
        warns[0].contains("Builtin function 'harness.llm.catalog' expects 0 arguments, got 1"),
        "{warns:?}"
    );
}

#[test]
fn test_len_accepts_nil_like_runtime() {
    let errs = errors(r"pipeline t(task) { let n: int = len(nil) }");
    assert!(errs.is_empty(), "got errors: {errs:?}");
}

#[test]
fn test_llm_call_option_literal_checks_known_field_types() {
    let errs = errors(
        r#"pipeline t(task) {
  llm_call("prompt", nil, {provider: "mock", max_tokens: "many"})
}"#,
    );
    assert_eq!(errs.len(), 1, "got errors: {errs:?}");
    assert!(errs[0].contains("argument 3 `options`"), "{errs:?}");
    assert!(errs[0].contains("max_tokens?: int"), "{errs:?}");
    assert!(errs[0].contains("max_tokens: string"), "{errs:?}");
}

#[test]
fn test_llm_call_option_literal_flags_probable_typos() {
    let warns = warnings(
        r#"pipeline t(task) {
  llm_call("prompt", nil, {provider: "mock", max_toknes: 256})
}"#,
    );
    assert_eq!(warns.len(), 1, "got warnings: {warns:?}");
    assert!(
        warns[0].contains("unknown `llm_call` option `max_toknes`"),
        "{warns:?}"
    );
    assert!(warns[0].contains("max_tokens"), "{warns:?}");
}

#[test]
fn test_llm_call_option_literal_accepts_openai_responses_options() {
    let warns = warnings(
        r#"pipeline t(task) {
  llm_call("prompt", nil, {
    provider: "mock",
    api_mode: "responses",
    provider_tools: [{type: "web_search_preview"}],
    previous_response_id: "resp_prev",
    response_store: true,
    background: false,
    truncation: "auto",
    compact: true,
    include: ["reasoning.encrypted_content"],
    max_tool_calls: 2
  })
}"#,
    );
    assert!(warns.is_empty(), "got warnings: {warns:?}");
}

#[test]
fn test_llm_call_option_literal_accepts_runtime_reasoning_and_routing_options() {
    let warns = warnings(
        r#"pipeline t(task) {
  let profile = {prompt_fragments: [{body: "Use repo context.", requires_caps: ["language.rust"]}], caps: ["language.rust"]}
  llm_call("prompt", nil, {
    provider: "mock",
    model_role: "merge",
    role: "fast_apply",
    routing: {chain: [{provider: "mock", model: "mock"}]},
    context_profile: profile,
    project_context_profile: profile,
    caps: ["language.rust"],
    capabilities: {tools: true},
    reasoning_policy: "off",
    thinking_policy: "auto",
    reasoning_scale: "small",
    problem_scale: "large",
    reasoning_task: "code",
    task_kind: "agent",
    task: "verify",
    timeout_ms: 250,
    fast: false,
    speed: "standard",
    video: false,
    reminders: {providers: []}
  })
  llm_completion("pre", "post", nil, {
    provider: "mock",
    reasoning_policy: "off",
    timeout_ms: 250
  })
}"#,
    );
    assert!(warns.is_empty(), "got warnings: {warns:?}");
}

#[test]
fn test_structured_llm_options_accept_structured_aliases() {
    let warns = warnings(
        r#"pipeline t(task) {
  let schema = {type: "object", properties: {answer: {type: "string"}}}
  llm_call_structured_result("prompt", schema, {
    provider: "mock",
    model_role: "merge",
    retries: 1,
    repair: {enabled: true, model: "mock"},
    output_validation: "error",
    reasoning_policy: "off",
    thinking_policy: "auto",
    reasoning_scale: "small",
    problem_scale: "large",
    reasoning_task: "code",
    task_kind: "agent",
    timeout_ms: 250,
    fast: false,
    video: false
  })
}"#,
    );
    assert!(warns.is_empty(), "got warnings: {warns:?}");
}

#[test]
fn test_builtin_arity_warning() {
    let warns = warnings(r#"pipeline t(task) { len("abc", "extra") }"#);
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("Builtin function 'len' expects 1 argument, got 2"));
}

#[test]
fn test_workflow_and_transcript_builtins_are_known() {
    let errs = errors(
        r#"pipeline t(task) {
  let flow = workflow_graph({name: "demo", entry: "act", nodes: {act: {kind: "stage"}}})
  let report: dict = workflow_policy_report(flow, {tools: tool_registry(), capabilities: {workspace: ["read_text"]}})
  let run: dict = workflow_execute("task", flow, [], {})
  let tree: dict = load_run_tree("run.json")
  let fixture: dict = run_record_fixture(run?.run)
  let suite: dict = run_record_eval_suite([{run: run?.run, fixture: fixture}])
  let diff: dict = run_record_diff(run?.run, run?.run)
  let manifest: dict = eval_suite_manifest({cases: [{run_path: "run.json"}]})
  let suite_report: dict = eval_suite_run(manifest)
  let wf: dict = artifact_workspace_file("src/main.rs", "fn main() {}", {source: "host"})
  let snap: dict = artifact_workspace_snapshot(["src/main.rs"], "snapshot")
  let selection: dict = artifact_editor_selection("src/main.rs", "main")
  let verify: dict = artifact_verification_result("verify", "ok")
  let test_result: dict = artifact_test_result("tests", "pass")
  let cmd: dict = artifact_command_result("cargo test", {status: 0})
  let patch: dict = artifact_diff("src/main.rs", "old", "new")
  let git: dict = artifact_git_diff("diff --git a b")
  let review: dict = artifact_diff_review(patch, "review me")
  let decision: dict = artifact_review_decision(review, "accepted")
  let proposal: dict = artifact_patch_proposal(review, "*** Begin Patch")
  let bundle: dict = artifact_verification_bundle("checks", [{name: "fmt", ok: true}])
  let apply: dict = artifact_apply_intent(review, "apply")
  let transcript = transcript_reset({metadata: {source: "test"}})
  let visible: string = transcript_render_visible(transcript_archive(transcript))
  let events: list = transcript_events(transcript)
  let worker: dict = worker_trigger({id: "worker_1"}, {follow_up: "next"})
  let context: string = artifact_context([], {max_artifacts: 1})
  __io_println(report)
  __io_println(run)
  __io_println(tree)
  __io_println(fixture)
  __io_println(suite)
  __io_println(diff)
  __io_println(manifest)
  __io_println(suite_report)
  __io_println(wf)
  __io_println(snap)
  __io_println(selection)
  __io_println(verify)
  __io_println(test_result)
  __io_println(cmd)
  __io_println(patch)
  __io_println(git)
  __io_println(review)
  __io_println(decision)
  __io_println(proposal)
  __io_println(bundle)
  __io_println(apply)
  __io_println(visible)
  __io_println(events)
  __io_println(worker)
  __io_println(context)
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_structured_stdlib_return_shapes_allow_documented_field_access() {
    let errs = errors(
        r#"pipeline t(task) {
  let call = llm_call("prompt", "system")
  let text: string = call.text
  let tool_calls: list = call.tool_calls
  let input_tokens: int = call.usage.input_tokens

  let safe = llm_call_safe("prompt", "system")
  let ok: bool = safe.ok
  let safe_text: string | nil = safe.response?.text
  let safe_error: string | nil = safe.error?.message
  let safe_status: int | nil = safe.error?.status

  let completion = llm_completion("prefix", "suffix", "system")
  let stop: string | nil = completion.stop_reason

  let snap = agent_session_snapshot("session")
  let length: int = snap.length
  let messages: list = snap.messages
  let created: string = snap.created_at

  let child = sub_agent_run("summarize this")
  let summary: string | nil = child?.summary
  let worker_id: string | nil = child?.id

  let transcript = transcript_reset({metadata: {source: "test"}})
  let transcript_id: string = transcript.id
  let archived_state: string | nil = transcript_archive(transcript).state

  let tools = tool_registry()
  let tool_type: string = tools._type
  let tool_entries: list = tools.tools

  __io_println(text)
  __io_println(tool_calls)
  __io_println(input_tokens)
  __io_println(ok)
  __io_println(safe_text)
  __io_println(safe_error)
  __io_println(stop)
  __io_println(length)
  __io_println(messages)
  __io_println(created)
  __io_println(summary)
  __io_println(worker_id)
  __io_println(transcript_id)
  __io_println(archived_state)
  __io_println(tool_type)
  __io_println(tool_entries)
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_structured_stdlib_return_shapes_type_known_fields() {
    let errs = errors(
        r#"pipeline t(task) {
  let call = llm_call("prompt", "system")
  let bad: int = call.text
}"#,
    );
    assert_eq!(errs.len(), 1, "expected one type error: {errs:?}");
    assert!(
        errs[0].contains("expected int") && errs[0].contains("found string"),
        "unexpected error: {errs:?}"
    );
}

#[test]
fn test_binary_op_type_inference() {
    let errs = errors("pipeline t(task) { let x: string = 1 + 2 }");
    assert_eq!(errs.len(), 1);
}

#[test]
fn test_exponentiation_requires_numeric_operands() {
    let errs = errors(r#"pipeline t(task) { let x = "nope" ** 2 }"#);
    assert!(
        errs.iter().any(|err| err.contains("can't use '**'")),
        "missing exponentiation type error: {errs:?}"
    );
}

#[test]
fn test_comparison_returns_bool() {
    let errs = errors("pipeline t(task) { let x: bool = 1 < 2 }");
    assert!(errs.is_empty());
}

#[test]
fn test_int_float_promotion() {
    let errs = errors("pipeline t(task) { let x: float = 42 }");
    assert!(errs.is_empty());
}

#[test]
fn test_untyped_code_no_errors() {
    let errs = errors(
        r#"pipeline t(task) {
  fn process(data) {
let result = data + " processed"
return result
  }
  log(process("hello"))
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_type_alias() {
    let errs = errors(
        r#"pipeline t(task) {
  type Name = string
  let x: Name = "hello"
}"#,
    );
    assert!(errs.is_empty());
}

#[test]
fn test_type_alias_mismatch() {
    let errs = errors(
        r"pipeline t(task) {
  type Name = string
  let x: Name = 42
}",
    );
    assert_eq!(errs.len(), 1);
}

#[test]
fn test_assignment_type_check() {
    let errs = errors(
        r#"pipeline t(task) {
  var x: int = 0
  x = "hello"
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("expected int, found string"));
}

#[test]
fn test_type_mismatch_render_snapshot_with_coercion_help() {
    crate::diagnostic::set_color_override(Some(false));
    let source = r"pipeline t(task) {
  let label: string = 42
}";
    let diags = check_source_with_source(source);
    let rendered = crate::diagnostic::render_type_diagnostic(source, "test.harn", &diags[0]);
    assert_eq!(
        rendered,
        r"error[HARN-TYP-007]: let binding `label`: expected string, found int
  --> test.harn:2:23
   |
 2 |   let label: string = 42
   |                       ^^ found this type
   = help: did you mean `to_string(42)`?
   = repair: casts/insert-explicit-conversion [scope-local] — Insert an explicit conversion or correct the operand type
   = note: expected type declared here
  --> test.harn:2:3
   |
 2 |   let label: string = 42
   |   ^^^^^^^^^^^^^^^^^^^^^^ expected type declared here
"
    );
}

#[test]
fn test_type_mismatch_render_snapshot_with_nested_note() {
    crate::diagnostic::set_color_override(Some(false));
    let source = r"pipeline t(task) {
  let item: {name: string, count: int} = {name: 1, count: 2}
}";
    let diags = check_source_with_source(source);
    let rendered = crate::diagnostic::render_type_diagnostic(source, "test.harn", &diags[0]);
    assert_eq!(
        rendered,
        r"error[HARN-TYP-007]: let binding `item`: expected {name: string, count: int}, found {name: int, count: int} (field 'name' has type int, expected string)
  --> test.harn:2:42
   |
 2 |   let item: {name: string, count: int} = {name: 1, count: 2}
   |                                          ^^^^^^^^^^^^^^^^^^^ found this type
   = repair: casts/insert-explicit-conversion [scope-local] — Insert an explicit conversion or correct the operand type
   = note: expected type declared here
  --> test.harn:2:3
   |
 2 |   let item: {name: string, count: int} = {name: 1, count: 2}
   |   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected type declared here
   = note: nested mismatch: field `name` expected string, found int
  --> test.harn:2:42
   |
 2 |   let item: {name: string, count: int} = {name: 1, count: 2}
   |                                          ^^^^^^^^^^^^^^^^^^^ nested mismatch: field `name` expected string, found int
"
    );
}

#[test]
fn test_covariance_int_to_float_in_fn() {
    let errs =
        errors("pipeline t(task) { fn scale(x: float) -> float { return x * 2.0 }\nscale(42) }");
    assert!(errs.is_empty());
}

#[test]
fn test_covariance_return_type() {
    let errs = errors("pipeline t(task) { fn get() -> float { return 42 } }");
    assert!(errs.is_empty());
}

#[test]
fn test_no_contravariance_float_to_int() {
    let errs = errors("pipeline t(task) { fn add(a: int) -> int { return a + 1 }\nadd(3.14) }");
    assert_eq!(errs.len(), 1);
}

// --- Comprehensive variance (issue #34) --------------------------------

#[test]
fn test_fn_param_contravariance_positive() {
    // A closure that accepts a float (a supertype of int) can
    // stand in for an expected `fn(int) -> int`: anything the
    // caller hands in (an int) the closure can still accept.
    let errs = errors(
        r"pipeline t(task) {
            let wide = fn(x: float) { return 0 }
            let cb: fn(int) -> int = wide
        }",
    );
    assert!(
        errs.is_empty(),
        "expected fn(float)->int to satisfy fn(int)->int, got: {errs:?}"
    );
}

#[test]
fn test_fn_param_contravariance_negative() {
    // A closure that only accepts ints cannot stand in for an
    // expected `fn(float) -> int`: the caller may hand it a
    // float, which it is not prepared to receive.
    let errs = errors(
        r"pipeline t(task) {
            let narrow = fn(x: int) { return 0 }
            let cb: fn(float) -> int = narrow
        }",
    );
    assert!(
        !errs.is_empty(),
        "expected fn(int)->int NOT to satisfy fn(float)->int, but type-check passed"
    );
}

#[test]
fn test_list_invariant_int_to_float_rejected() {
    // `list<int>` must not flow into `list<float>` — lists are
    // mutable, so a covariant assignment is unsound.
    let errs = errors(
        r"pipeline t(task) {
            let xs: list<int> = [1, 2, 3]
            let ys: list<float> = xs
        }",
    );
    assert!(
        !errs.is_empty(),
        "expected list<int> NOT to flow into list<float>, but type-check passed"
    );
}

#[test]
fn test_iter_covariant_int_to_float_accepted() {
    // Iterators are read-only, so element-type widening is sound.
    let errs = errors(
        r"pipeline t(task) {
            fn sink(ys: iter<float>) -> int { return 0 }
            fn pipe(xs: iter<int>) -> int { return sink(xs) }
        }",
    );
    assert!(
        errs.is_empty(),
        "expected iter<int> to flow into iter<float>, got: {errs:?}"
    );
}

#[test]
fn test_decl_site_out_used_in_contravariant_position_rejected() {
    // `type Box<out T> = fn(T) -> ()` — T is declared covariant
    // but appears only as an input (contravariant). Must be
    // rejected at declaration time.
    let errs = errors(
        r"pipeline t(task) {
            type Box<out T> = fn(T) -> int
        }",
    );
    assert!(
        errs.iter().any(|e| e.contains("declared 'out'")),
        "expected 'out T' misuse diagnostic, got: {errs:?}"
    );
}

#[test]
fn test_decl_site_in_used_in_covariant_position_rejected() {
    // `interface Producer<in T> { fn next() -> T }` — T is declared
    // contravariant but appears only in output position.
    let errs = errors(
        r"pipeline t(task) {
            interface Producer<in T> { fn next() -> T }
        }",
    );
    assert!(
        errs.iter().any(|e| e.contains("declared 'in'")),
        "expected 'in T' misuse diagnostic, got: {errs:?}"
    );
}

#[test]
fn test_decl_site_out_in_covariant_position_ok() {
    // `type Reader<out T> = fn() -> T` — T appears in a covariant
    // position, consistent with `out T`.
    let errs = errors(
        r"pipeline t(task) {
            type Reader<out T> = fn() -> T
        }",
    );
    assert!(
        errs.iter().all(|e| !e.contains("declared 'out'")),
        "unexpected variance diagnostic: {errs:?}"
    );
}

#[test]
fn test_dict_invariant_int_to_float_rejected() {
    let errs = errors(
        r#"pipeline t(task) {
            let d: dict<string, int> = {"a": 1}
            let e: dict<string, float> = d
        }"#,
    );
    assert!(
        !errs.is_empty(),
        "expected dict<string, int> NOT to flow into dict<string, float>"
    );
}

#[test]
fn test_generic_alias_distributes_over_closed_literal_union() {
    // `ActionContainer<Action>` must distribute into
    // `ActionContainer<"create"> | ActionContainer<"edit">`, which lets a
    // `fn("create") -> nil` value flow into the `"create"` branch without
    // running into contravariance grief (the TypeScript playground bug).
    let errs = errors(
        r#"
type Action = "create" | "edit"
type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }

fn process_create(a: "create") {}
fn process_edit(a: "edit") {}

pipeline t(task) {
    let c: ActionContainer<Action> = {action: "create", process_action: process_create}
    let d: ActionContainer<Action> = {action: "edit",   process_action: process_edit}
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_bare_function_reference_infers_fn_type() {
    // Before the identifier-to-fn-type fallback, a bare function reference
    // used as a value inferred to `None`, which meant it collapsed to
    // `nil` when placed into a dict literal. That silently broke
    // assignability against any typed `fn(...) -> R` slot.
    let errs = errors(
        r"
fn process(a: string) -> string { return a }

pipeline t(task) {
    let slot: fn(string) -> string = process
    let d: { handler: fn(string) -> string } = { handler: process }
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_contextual_closure_typing_checks_annotated_binding_body() {
    let errs = errors(
        r#"pipeline t(task) {
            let f: fn(int) -> int = { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure body error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_assignment_body() {
    let errs = errors(
        r#"pipeline t(task) {
            var f: fn(int) -> int = { x -> x + 1 }
            f = { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure assignment error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_call_argument_body() {
    let errs = errors(
        r#"fn keep_callback(f: fn(int) -> int) -> int { return 0 }

pipeline t(task) {
    keep_callback({ x -> x + "oops" })
}"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure argument error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_uses_bound_generic_call_argument() {
    let errs = errors(
        r#"fn use_callback<T>(seed: T, f: fn(T) -> T) -> int { return 0 }

pipeline t(task) {
    use_callback(1, { x -> x + "oops" })
}"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual generic closure argument error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_skips_unbound_generic_call_argument() {
    let errs = errors(
        r#"fn keep_callback<T>(f: fn(T) -> T) -> int { return 0 }

pipeline t(task) {
    keep_callback({ x -> x + "oops" })
}"#,
    );
    assert!(
        errs.is_empty(),
        "unbound generic callback parameters should remain gradual: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_default_param_body() {
    let errs = errors(
        r#"fn use_callback(f: fn(int) -> int = { x -> x + "oops" }) -> int {
            return 0
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual default parameter error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_return_slot_body() {
    let errs = errors(
        r#"fn make_callback() -> fn(int) -> int {
            return { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure return error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_implicit_return_slot_body() {
    let errs = errors(
        r#"fn make_callback() -> fn(int) -> int {
            { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure implicit return error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_pipeline_return_slot_body() {
    let errs = errors(
        r#"pipeline make_callback(task) -> fn(int) -> int {
            return { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual pipeline return error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_pipeline_implicit_return_slot_body() {
    let errs = errors(
        r#"pipeline make_callback(task) -> fn(int) -> int {
            { x -> x + "oops" }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual pipeline implicit return error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_shape_field_body() {
    let errs = errors(
        r#"pipeline t(task) {
            let slot: { callback: fn(int) -> int } = { callback: { x -> x + "oops" } }
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual closure field error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_checks_collection_method_body() {
    let errs = errors(
        r#"pipeline t(task) {
            let xs: list<int> = [1, 2, 3]
            xs.map({ x -> x + "oops" })
        }"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("can't add int and string")),
        "expected contextual collection closure error, got: {errs:?}"
    );
}

#[test]
fn test_contextual_collection_method_keeps_unknown_parameter_lenient() {
    let warns = warnings(
        r#"fn grid_expand(state) {
            let moves = [
                {name: "right", x: state.x + 1},
                {name: "left", x: state.x - 1},
            ]
            return moves.filter({ move -> move.x >= 0 && move.x <= 2 })
        }"#,
    );
    assert!(
        warns.iter().all(|warn| !warn.contains("Comparison")),
        "unknown collection element types should not produce comparison warnings: {warns:?}"
    );
}

#[test]
fn test_contextual_collection_method_keeps_any_parameter_lenient() {
    let errs = errors(
        r"pipeline t(task) {
            let xs = [1, 2, 3]
            iter(xs).map({ x -> x * 10 }).to_list()
        }",
    );
    assert!(
        errs.is_empty(),
        "`any` collection element types should remain gradual: {errs:?}"
    );
}

#[test]
fn test_contextual_closure_typing_skips_any_callback_parameter() {
    let errs = errors(
        r"fn wait_for(condition: fn(any) -> bool) -> int { return 0 }

pipeline t(task) {
    wait_for({ state -> state.ready })
}",
    );
    assert!(
        errs.is_empty(),
        "`any` callback parameters should remain gradual: {errs:?}"
    );
}

#[test]
fn test_harness_term_methods_infer_concrete_types() {
    let errs = errors(
        r#"
fn main(harness: Harness) {
    let width: int = harness.term.width()
    let height: int = harness.term.height()
    let password: string = harness.term.read_password("password: ")
}
"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");

    let bad_errs = errors(
        r#"
fn main(harness: Harness) {
    let width: string = harness.term.width()
    let password: int = harness.term.read_password("password: ")
}
"#,
    );
    assert_eq!(bad_errs.len(), 2, "expected two errors: {bad_errs:?}");
    assert!(
        bad_errs
            .iter()
            .any(|error| error.contains("expected string") && error.contains("found int")),
        "expected width mismatch, got: {bad_errs:?}"
    );
    assert!(
        bad_errs
            .iter()
            .any(|error| error.contains("expected int") && error.contains("found string")),
        "expected password mismatch, got: {bad_errs:?}"
    );
}

#[test]
fn test_generic_alias_distribution_preserves_non_union_arg() {
    // Non-union arguments still substitute plainly: `ActionContainer<int>`
    // expands to `{ action: int, process_action: fn(int) -> nil }` with no
    // distribution. A `fn(int) -> nil` handler fits; a `fn(string) -> nil`
    // does not.
    let ok_errs = errors(
        r"
type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }

fn process_int(a: int) {}

pipeline t(task) {
    let c: ActionContainer<int> = {action: 7, process_action: process_int}
}",
    );
    assert!(ok_errs.is_empty(), "expected no errors: {ok_errs:?}");

    let bad_errs = errors(
        r"
type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }

fn process_string(a: string) {}

pipeline t(task) {
    let c: ActionContainer<int> = {action: 7, process_action: process_string}
}",
    );
    assert!(
        !bad_errs.is_empty(),
        "expected an error: `fn(string)` cannot fill an `fn(int)` slot"
    );
}
