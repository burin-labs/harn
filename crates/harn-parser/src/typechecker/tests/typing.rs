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
    // rest let is refined to a `list<…>` (not left untyped/gradual). Likewise
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
fn test_generic_rest_return_type_binds_from_all_rest_args() {
    let errs = errors(
        r#"pipeline t(task) {
  fn wrap<T>(label: string, ...xs: list<T>) -> list<T> { return [] }
  let bad: list<string> = wrap("items", [], [1])
}"#,
    );
    assert_eq!(errs.len(), 1, "expected one mismatch, got: {errs:?}");
    assert!(errs[0].contains("expected list<string>"), "{errs:?}");
    assert!(errs[0].contains("found list<int>"), "{errs:?}");
}

#[test]
fn test_transcript_append_builtins_preserve_input_container_type() {
    let errs = errors(
        r#"pipeline t(task) {
  let built = transcript({workflow: "demo"})
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
  log(names?.[0])
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
fn test_nil_coalesce_unreachable_fallback_warns_on_non_nil_typed_producer() {
    let diagnostics = check_source_with_source(
        r#"
fn parse_number(raw: string) -> float {
  return 1.0
}

pipeline t(task) {
  let raw: string? = nil
  const value = parse_number(raw ?? "0") ?? 0.0
  log(value)
}
"#,
    );
    let unreachable = diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.details,
                Some(DiagnosticDetails::LintRule { rule })
                    if *rule == "nil-coalesce-unreachable-fallback"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(unreachable.len(), 1, "got diagnostics: {diagnostics:?}");
    assert_eq!(
        unreachable[0].code,
        crate::diagnostic_codes::Code::LintNilCoalesceUnreachableFallback
    );
    assert_eq!(
        unreachable[0]
            .fix
            .as_ref()
            .expect("fix")
            .first()
            .expect("single fix")
            .replacement,
        ""
    );
}

#[test]
fn test_nil_coalesce_unreachable_fallback_respects_nilable_left_side() {
    let diagnostics = check_source_with_source(
        r"
pipeline t(task) {
  let maybe: int? = nil
  const value = maybe ?? 0
  log(value)
}
",
    );
    assert!(
        diagnostics.iter().all(|diagnostic| !matches!(
            &diagnostic.details,
            Some(DiagnosticDetails::LintRule { rule })
                if *rule == "nil-coalesce-unreachable-fallback"
        )),
        "nilable left side should not trigger unreachable-fallback lint: {diagnostics:?}"
    );
}

#[test]
fn test_nil_coalesce_unreachable_fallback_ignores_non_producer_left_side() {
    let diagnostics = check_source_with_source(
        r#"
pipeline t(task) {
  const present = "value"
  const from_local = present ?? "fallback"
  const from_literal = "present" ?? "fallback"
  log(from_local + from_literal)
}
"#,
    );
    assert!(
        diagnostics.iter().all(|diagnostic| !matches!(
            &diagnostic.details,
            Some(DiagnosticDetails::LintRule { rule })
                if *rule == "nil-coalesce-unreachable-fallback"
        )),
        "non-producer left sides should not trigger unreachable-fallback lint: {diagnostics:?}"
    );
}

#[test]
fn test_nil_coalesce_unreachable_fallback_ignores_ambient_typed_producer() {
    let diagnostics = check_source_with_source(
        r"
pipeline t(task) {
  const value = to_float(1) ?? 0.0
  log(value)
}
",
    );
    assert!(
        diagnostics.iter().all(|diagnostic| !matches!(
            &diagnostic.details,
            Some(DiagnosticDetails::LintRule { rule })
                if *rule == "nil-coalesce-unreachable-fallback"
        )),
        "ambient typed producers should not trigger unreachable-fallback lint: {diagnostics:?}"
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
  let x: int = 0
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
fn test_list_covariant_int_to_float_accepted() {
    // `list<int>` flows into `list<float>`: `list` is covariant in its element
    // type. The classic covariance-with-mutation hole needs shared mutable
    // aliasing, which Harn does not have — values have copy semantics, so the
    // `list<float>` binding gets an independent copy and nothing the original
    // `list<int>` observes can change. (See the `list` arm in `subtyping.rs`.)
    let errs = errors(
        r"pipeline t(task) {
            let xs: list<int> = [1, 2, 3]
            let ys: list<float> = xs
        }",
    );
    assert!(
        errs.is_empty(),
        "expected list<int> to flow into list<float> under value semantics, got: {errs:?}"
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
fn test_dict_covariant_value_int_to_float_accepted() {
    // dict<K, V> is covariant in its value type V for the same value-semantics
    // reason as `list`: no shared mutable aliasing, so a widening read is sound.
    let errs = errors(
        r#"pipeline t(task) {
            let d: dict<string, int> = {"a": 1}
            let e: dict<string, float> = d
        }"#,
    );
    assert!(
        errs.is_empty(),
        "expected dict<string, int> value to widen into dict<string, float>, got: {errs:?}"
    );
}

#[test]
fn test_dict_key_stays_invariant() {
    // The value type widens (covariant) but the key type does not: a
    // `dict<string, int>` must not flow into a `dict<int, int>`. Key variance
    // interacts with lookup in ways plain width-subtyping does not, so keys stay
    // exact.
    let errs = errors(
        r#"pipeline t(task) {
            let d: dict<string, int> = {"a": 1}
            let e: dict<int, int> = d
        }"#,
    );
    assert!(
        !errs.is_empty(),
        "expected dict<string, int> NOT to flow into dict<int, int> (keys invariant)"
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
            let f: fn(int) -> int = { x -> x + 1 }
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

#[test]
fn test_bare_variant_match_patterns_bind_and_cover() {
    // Bare `Ok(v)` / `Err(e)` patterns resolve to the Result enum, bind
    // instantiated payload types, and count toward exhaustiveness.
    let errs = errors(
        r"fn g() -> Result<int, string> { return Ok(1) }

fn f() -> int {
  match g() {
    Ok(v) -> { return v }
    Err(e) -> { return e.len() }
  }
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");

    // Missing a variant is still non-exhaustive with bare patterns.
    let errs = errors(
        r"fn g() -> Result<int, string> { return Ok(1) }

fn f() -> int {
  match g() {
    Ok(v) -> { return v }
  }
}",
    );
    assert!(
        errs.iter().any(|e| e.contains("Non-exhaustive")),
        "expected non-exhaustive error, got: {errs:?}"
    );
}

#[test]
fn test_bare_variant_pattern_on_user_enum() {
    let errs = errors(
        r"enum Shape {
  Circle(radius: int),
  Square(side: int)
}

fn area(s: Shape) -> int {
  match s {
    Circle(r) -> { return r * r * 3 }
    Square(w) -> { return w * w }
  }
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn test_ternary_branch_merge_collapses_never_and_simplifies() {
    // A throwing arm contributes `never`, which must collapse out of the
    // merged type — same rule as if/else expressions.
    let errs = errors(
        r#"fn f(flag: bool) -> int {
  let x = flag ? 1 : unreachable("boom")
  return x
}"#,
    );
    assert!(errs.is_empty(), "never arm should collapse: {errs:?}");

    // Nested unions flatten: `(int | nil) : int` is `int | nil`, not a
    // nested union that defeats downstream nil-narrowing.
    let errs = errors(
        r"fn g() -> int? { return 1 }

fn f(flag: bool) -> int {
  let x = flag ? g() : 0
  if x != nil {
    return x
  }
  return -1
}",
    );
    assert!(errs.is_empty(), "flattened union should narrow: {errs:?}");
}

#[test]
fn test_aliased_dict_receiver_keeps_value_type_across_methods() {
    // `type Env = dict<string, string>`: every dict combinator must see
    // through the alias, not just `.values()`.
    let errs = errors(
        r"type Env = dict<string, string>

fn f(e: Env) -> int {
  let m: dict<string, string> = e.map_values(fn(v) { return v })
  return m.count()
}",
    );
    assert!(errs.is_empty(), "map_values lost the alias types: {errs:?}");

    let errs = errors(
        r"type Env = dict<string, string>

fn f(e: Env) -> dict<string, int> {
  return e.map_values(fn(v) { return v })
}",
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("dict<string, int>") && e.contains("dict<string, string>")),
        "aliased receiver value type must participate in checks: {errs:?}"
    );
}

#[test]
fn test_bool_match_requires_both_arms() {
    let errs = errors(
        r"fn f(b: bool) -> int {
  match b {
    true -> { return 1 }
  }
}",
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("Non-exhaustive match on bool") && e.contains("false")),
        "expected bool exhaustiveness error: {errs:?}"
    );

    let errs = errors(
        r"fn f(b: bool) -> int {
  match b {
    true -> { return 1 }
    false -> { return 0 }
  }
}",
    );
    assert!(
        errs.is_empty(),
        "covered bool match should be clean: {errs:?}"
    );
}

#[test]
fn recursive_typed_parameter_self_call_terminates() {
    let errs = errors(
        r"type Tree = {value: int, children: list<Tree>}

fn total(t: Tree) -> int {
  let sum = t.value
  for child in t.children {
    sum = sum + total(child)
  }
  return sum
}

pipeline default() {
  const tree: Tree = {value: 1, children: [{value: 2, children: []}]}
  total(tree)
}",
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}

#[test]
fn recursive_typed_parameter_still_rejects_nested_mismatch() {
    let errs = errors(
        r#"type Tree = {value: int, children: list<Tree>}

fn visit(t: Tree) -> int {
  return visit({value: "wrong", children: []})
}"#,
    );
    assert!(
        errs.iter()
            .any(|error| error.contains("expected int") && error.contains("found string")),
        "expected nested recursive-alias mismatch, got: {errs:?}"
    );
}

#[test]
fn alias_forwarding_chain_reaches_structural_root() {
    let errs = errors(
        r#"type Binding = {id: string}
type Handle = Binding

fn dynamic_handle() -> Binding {
  return {id: "trigger-1"}
}

pipeline default() {
  const handle: Handle = dynamic_handle()
  return handle.id
}"#,
    );
    assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
}
