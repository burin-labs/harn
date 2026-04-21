//! Basic typing: literals, fn / pipeline signatures, generics, type aliases, variance.

use super::*;

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
    assert!(errs[0].contains("declared as int"));
    assert!(errs[0].contains("assigned string"));
}

#[test]
fn test_correct_typed_fn() {
    let errs =
        errors("pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }\nadd(1, 2) }");
    assert!(errs.is_empty());
}

#[test]
fn test_fn_arg_type_mismatch() {
    let errs = errors(
        r#"pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }
add("hello", 2) }"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("Argument 1"));
    assert!(errs[0].contains("expected int"));
}

#[test]
fn test_return_type_mismatch() {
    let errs = errors(r#"pipeline t(task) { fn get() -> int { return "hello" } }"#);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("return type doesn't match"));
}

#[test]
fn test_union_type_compatible() {
    let errs = errors(r#"pipeline t(task) { let x: string | nil = nil }"#);
    assert!(errs.is_empty());
}

#[test]
fn test_union_type_mismatch() {
    let errs = errors(r#"pipeline t(task) { let x: string | nil = 42 }"#);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("declared as"));
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
    assert!(errs[0].contains("declared as nil"), "got: {}", errs[0]);
}

#[test]
fn test_type_inference_propagation() {
    let errs = errors(
        r#"pipeline t(task) {
  fn add(a: int, b: int) -> int { return a + b }
  let result: string = add(1, 2)
}"#,
    );
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("declared as"));
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
fn test_generic_type_param_must_bind_consistently() {
    let errs = errors(
        r#"pipeline t(task) {
  fn keep<T>(a: T, b: T) -> T { return a }
  keep(1, "x")
}"#,
    );
    assert_eq!(errs.len(), 2, "expected 2 errors, got: {:?}", errs);
    assert!(
        errs.iter()
            .any(|err| err.contains("type parameter 'T' was inferred as both int and string")),
        "missing generic binding conflict error: {:?}",
        errs
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("Argument 2 ('b'): expected int, got string")),
        "missing instantiated argument mismatch error: {:?}",
        errs
    );
}

#[test]
fn test_generic_list_binding_propagates_element_type() {
    let errs = errors(
        r#"pipeline t(task) {
  fn first<T>(items: list<T>) -> T { return items[0] }
  let bad: string = first([1, 2, 3])
}"#,
    );
    assert_eq!(errs.len(), 1, "expected 1 error, got: {:?}", errs);
    assert!(errs[0].contains("declared as string, but assigned int"));
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
fn test_unknown_struct_literal_reports_error() {
    let diagnostics = check_source(
        r#"pipeline t(task) {
  let p = Point {x: 3, y: 4}
}"#,
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
        r#"pipeline t(task) {
  struct Point {
    x: int
    y: int
  }

  let p = Piont {x: 3, y: 4}
}"#,
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
        r#"pipeline t(task) {
  enum Option<T> {
Some(value: T),
None
  }
  let value: Option<int> = Option.Some(42)
}"#,
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
        r#"pipeline t(task) {
  let bad: Result<int, string> = Result.Err(42)
}"#,
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
  println(report)
  println(run)
  println(tree)
  println(fixture)
  println(suite)
  println(diff)
  println(manifest)
  println(suite_report)
  println(wf)
  println(snap)
  println(selection)
  println(verify)
  println(test_result)
  println(cmd)
  println(patch)
  println(git)
  println(review)
  println(decision)
  println(proposal)
  println(bundle)
  println(apply)
  println(visible)
  println(events)
  println(worker)
  println(context)
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
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
        r#"pipeline t(task) {
  type Name = string
  let x: Name = 42
}"#,
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
    assert!(errs[0].contains("can't assign string"));
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
        r#"pipeline t(task) {
            let wide = fn(x: float) { return 0 }
            let cb: fn(int) -> int = wide
        }"#,
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
        r#"pipeline t(task) {
            let narrow = fn(x: int) { return 0 }
            let cb: fn(float) -> int = narrow
        }"#,
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
        r#"pipeline t(task) {
            let xs: list<int> = [1, 2, 3]
            let ys: list<float> = xs
        }"#,
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
        r#"pipeline t(task) {
            fn sink(ys: iter<float>) -> int { return 0 }
            fn pipe(xs: iter<int>) -> int { return sink(xs) }
        }"#,
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
        r#"pipeline t(task) {
            type Box<out T> = fn(T) -> int
        }"#,
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
        r#"pipeline t(task) {
            interface Producer<in T> { fn next() -> T }
        }"#,
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
        r#"pipeline t(task) {
            type Reader<out T> = fn() -> T
        }"#,
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
        r#"
fn process(a: string) -> string { return a }

pipeline t(task) {
    let slot: fn(string) -> string = process
    let d: { handler: fn(string) -> string } = { handler: process }
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_generic_alias_distribution_preserves_non_union_arg() {
    // Non-union arguments still substitute plainly: `ActionContainer<int>`
    // expands to `{ action: int, process_action: fn(int) -> nil }` with no
    // distribution. A `fn(int) -> nil` handler fits; a `fn(string) -> nil`
    // does not.
    let ok_errs = errors(
        r#"
type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }

fn process_int(a: int) {}

pipeline t(task) {
    let c: ActionContainer<int> = {action: 7, process_action: process_int}
}"#,
    );
    assert!(ok_errs.is_empty(), "expected no errors: {ok_errs:?}");

    let bad_errs = errors(
        r#"
type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }

fn process_string(a: string) {}

pipeline t(task) {
    let c: ActionContainer<int> = {action: 7, process_action: process_string}
}"#,
    );
    assert!(
        !bad_errs.is_empty(),
        "expected an error: `fn(string)` cannot fill an `fn(int)` slot"
    );
}
