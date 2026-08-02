//! Call compatibility, generic instantiation, and typed builtin surfaces.

use super::*;

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
        errs[0].contains("argument 1 `options`: unknown field `dropnil` in closed record"),
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
fn test_inferred_record_call_keeps_width_subtyping() {
    let errs = errors(
        r#"fn greet(u: {name: string}) -> string {
  return "hi " + u.name
}

pipeline t(task) {
  const person = {name: "Bob", age: 25}
  greet(person)
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
  let hit = nil
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
  let hit = nil
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
  let hit: {name: string, score: int} | nil = nil
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
  let hit: nil = nil
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
    // `items[0]` is `T?` under index soundness, so an honest element accessor
    // is declared `-> T?`; the instantiated call then yields `string?`.
    let errs = errors(
        r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  fn first<T>(items: list<T>) -> T? { return items[0] }
  let n: int = identity(42)
  let s: string? = first(["a", "b"])
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
    // Explicit type arguments are a frozen contract: the argument is
    // checked against the instantiation (`expected int, found string`),
    // with no arg-driven re-inference (and so no union-join widening).
    let errs = errors(
        r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  let n: int = identity<int>("oops")
}"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected int, found string")),
        "missing explicit type-arg mismatch error: {errs:?}"
    );
    assert!(
        !errs.iter().any(|err| err.contains("inferred as both")),
        "explicit type args must not run arg-driven inference: {errs:?}"
    );
}

#[test]
fn test_generic_type_param_conflicting_candidates_join_to_union() {
    // Two arguments pinning the same parameter to different types infer
    // the union (`T = int | string`), matching how a heterogeneous list
    // literal infers `list<int | string>` and how TypeScript infers a
    // union for multiple inference candidates. The joined type still
    // participates in downstream checks.
    let errs = errors(
        r#"pipeline t(task) {
  fn keep<T>(a: T, b: T) -> T { return a }
  keep(1, "x")
}"#,
    );
    assert!(errs.is_empty(), "union-join call should be clean: {errs:?}");

    let errs = errors(
        r#"pipeline t(task) {
  fn keep<T>(a: T, b: T) -> T { return a }
  let n: int = keep(1, "x")
}"#,
    );
    assert!(
        errs.iter()
            .any(|err| err.contains("expected int, found int | string")),
        "joined union must flow to the return type: {errs:?}"
    );
}

#[test]
fn test_generic_list_binding_propagates_element_type() {
    // The element accessor is `-> T?` (index soundness): instantiating `T` to
    // `int` still surfaces the callsite mismatch against `string`.
    let errs = errors(
        r"pipeline t(task) {
  fn first<T>(items: list<T>) -> T? { return items[0] }
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
fn test_result_match_pattern_binds_instantiated_payload_types() {
    // `Result<int, string>` scrutinee: `Result.Ok(v)` binds `v: int`, and
    // `Result.Err(e)` binds `e: string` — not the raw declaration params.
    let ok_errs = errors(
        r"fn g() -> Result<int, string> { return Ok(1) }

fn f() -> int {
  let r = g()
  match r {
    Result.Ok(v) -> { return v }
    Result.Err(e) -> { return e.len() }
  }
}",
    );
    assert!(ok_errs.is_empty(), "unexpected type errors: {ok_errs:?}");

    // The instantiated payload participates in real checks: returning the
    // `int` payload from a `string`-returning fn is a mismatch.
    let bad_errs = errors(
        r"fn g() -> Result<int, string> { return Ok(1) }

fn f() -> string {
  let r = g()
  match r {
    Result.Ok(v) -> { return v }
    Result.Err(e) -> { return e }
  }
}",
    );
    assert_eq!(bad_errs.len(), 1, "expected 1 error, got: {bad_errs:?}");
    assert!(bad_errs[0].contains("expected string, found int"));
}

#[test]
fn test_generic_enum_match_pattern_binds_instantiated_payload() {
    let errs = errors(
        r#"enum Box<T> {
  Full(value: T),
  Empty
}

fn f(b: Box<string>) -> string {
  match b {
    Box.Full(v) -> { return v }
    Box.Empty -> { return "" }
  }
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_unparameterised_generic_enum_match_binds_gradual_payload() {
    // A bare `Result` scrutinee (no type args statically known) must not
    // leak the phantom declaration param `T` into the arm scope; the
    // binding degrades to gradual and the arm body stays checkable.
    let errs = errors(
        r"fn f(r: Result) -> int {
  match r {
    Result.Ok(v) -> { return v }
    Result.Err(e) -> { return 0 }
  }
}",
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
fn test_pure_sha256_type_inference() {
    let errs = errors(
        r#"fn main(harness: Harness) {
  let digest: string = sha256_hex("")
  let wrong: int = sha256_hex("hello")
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
  let workspace_root: string = harness.fs.workspace_temp_dir()
  let workspace_dir: string = harness.fs.mkdtemp_in_workspace("harn-type-")
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
  let refresh: dict = harness.llm.catalog_refresh()
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
fn test_llm_call_option_literal_accepts_canonical_responses_options() {
    let warns = warnings(
        r#"pipeline t(task) {
  llm_call("prompt", nil, {
    provider: "mock", mock_scope: "completion.judge",
    api_mode: "responses",
    provider_tools: [{type: "web_search_preview"}],
    previous_response_id: "resp_prev",
    store: true,
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
fn test_llm_call_option_literal_accepts_canonical_reasoning_and_routing_options() {
    let warns = warnings(
        r#"pipeline t(task) {
  let profile = {prompt_fragments: [{body: "Use repo context.", requires_caps: ["language.rust"]}], caps: ["language.rust"]}
  llm_call("prompt", nil, {
    provider: "mock",
    model_role: "merge",
    routing: {chain: [{provider: "mock", model: "mock"}]},
    context_profile: profile,
    capabilities: {tools: true},
    reasoning_policy: "off",
    reasoning_scale: "small",
    reasoning_task: "code",
    timeout_ms: 250,
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
fn test_structured_llm_options_accept_canonical_keys() {
    let warns = warnings(
        r#"pipeline t(task) {
  let schema = {type: "object", properties: {answer: {type: "string"}}}
  llm_call_structured_result("prompt", schema, {
    provider: "mock",
    model_role: "merge",
    retries: 1,
    repair: {enabled: true, model: "mock"},
    effort: "high",
    reasoning_policy: "off",
    reasoning_scale: "small",
    reasoning_task: "code",
    timeout_ms: 250,
    speed: "standard",
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
fn test_user_function_named_like_builtin_reports_user_arity() {
    let warns = warnings(
        r"pipeline t(task) {
  fn len(value: string) -> int { return 0 }
  len()
}",
    );
    assert_eq!(warns.len(), 1, "expected one arity warning, got: {warns:?}");
    assert!(warns[0].contains("Function 'len' expects at least 1 argument, got 0"));
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
