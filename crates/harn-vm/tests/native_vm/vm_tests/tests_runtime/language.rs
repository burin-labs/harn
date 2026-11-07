//! Core language semantics executed end to end.
//!
//! Arithmetic and comparisons, control flow, functions and closures, the list
//! and dict builtins, string methods, list-pattern matching, the pipe operator,
//! nil coalescing, subscripting, and the guards on oversized string operations.

use super::harness::*;

#[test]
fn test_typed_opcode_drift_falls_back_to_generic() {
    // A value that drifts from its declared type must never surface a
    // specialization-internal error. Drift now enters through a parameter: since
    // harn#6252 an annotated binding is checked where it is written, so
    // `const x: int = "bad"` is rejected there instead of drifting into `x + 1`
    // (see `typed_annotation_is_enforced_at_the_binding_site` below).
    let err = run_vm_err(
        r#"pipeline t(harness: Harness, task: unknown) {
  fn add_one(n: int) { return n + 1 }
  harness.stdio.log(add_one("bad"))
}"#,
    );
    assert!(
        !err.contains("Typed int"),
        "typed opcodes must no longer surface specialization-internal errors: {err}"
    );
}

#[test]
fn typed_annotation_is_enforced_at_the_binding_site() {
    // The counterpart: a declared type on a binding rejects its initializer
    // rather than letting the wrong value flow into the rest of the body. The
    // error names the binding, so the report points at the declaration the
    // reader has to fix — not at whatever expression happened to consume it.
    let err = run_vm_err(
        r#"pipeline t(harness: Harness, task: unknown) {
  const x: int = "bad"
  harness.stdio.log(x + 1)
}"#,
    );
    assert!(
        err.contains("binding `x`") && err.contains("expects int"),
        "expected the binding-site type error, got: {err}"
    );
}

#[rstest::rstest]
// `-2 ** 2` is `-(2 ** 2) = -4`, matching Python/Ruby/math notation rather
// than the spreadsheet `(-2) ** 2 = 4` reading. The exponent operand still
// accepts a unary prefix, so `2 ** -2` stays `2 ** (-2)`.
#[case::test_unary_minus_binds_looser_than_exponent("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(-2 ** 2)\nharness.stdio.log(-2 ** 3)\nharness.stdio.log(2 ** -2)\nharness.stdio.log(-(2 ** 2)) }", "[harn] -4\n[harn] -8\n[harn] 0.25\n[harn] -4")]
#[case::test_comparisons("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(1 < 2)\nharness.stdio.log(2 > 3)\nharness.stdio.log(1 == 1)\nharness.stdio.log(1 != 2) }", "[harn] true\n[harn] false\n[harn] true\n[harn] true")]
#[case::test_fn_decl_and_call("pipeline t(harness: Harness, task: unknown) { fn add(a, b) { return a + b }\nharness.stdio.log(add(3, 4)) }", "[harn] 7")]
#[case::test_closure_capture("pipeline t(harness: Harness, task: unknown) { const base = 10\nfn offset(x) { return x + base }\nharness.stdio.log(offset(5)) }", "[harn] 15")]
// A fixed-arity list pattern matches ONLY that length: `[a, b]` must NOT
// match a 3-element list (previously it did via a `len >= 2` check).
#[case::test_match_list_pattern_is_exact_length(
    r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3] {
  [a, b] -> { harness.stdio.log("two: ${a},${b}") }
  _ -> { harness.stdio.log("other") }
} }"#,
    "[harn] other"
)]
#[case::test_match_list_pattern_exact_match_binds(
    r#"pipeline t(harness: Harness, task: unknown) {
match [10, 20] {
  [a, b] -> { harness.stdio.log("${a},${b}") }
  _ -> { harness.stdio.log("other") }
} }"#,
    "[harn] 10,20"
)]
#[case::test_match_list_pattern_rest_binds_tail(
    r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3] {
  [head, ...rest] -> { harness.stdio.log("${head} :: ${rest}") }
  _ -> { harness.stdio.log("none") }
} }"#,
    "[harn] 1 :: [2, 3]"
)]
#[case::test_match_list_pattern_rest_empty_tail(
    r#"pipeline t(harness: Harness, task: unknown) {
match [42] {
  [head, ...rest] -> { harness.stdio.log("${head} :: ${rest}") }
  _ -> { harness.stdio.log("none") }
} }"#,
    "[harn] 42 :: []"
)]
#[case::test_match_list_pattern_discard_rest_matches_at_least(
    r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3, 4] {
  [first, ..._] -> { harness.stdio.log("first=${first}") }
  _ -> { harness.stdio.log("empty") }
} }"#,
    "[harn] first=1"
)]
#[case::test_pipe_operator("pipeline t(harness: Harness, task: unknown) { fn double(x) { return x * 2 }\nlet r = 5 |> double\nharness.stdio.log(r) }", "[harn] 10")]
#[case::positive_assert_false_fallback_matches_native_truthiness(
    r"
fn direct_accepts(x: bool?) -> bool {
  return try {
    assert(x)
    true
  } catch {
    false
  }
}

fn coalesced_accepts(x: bool?) -> bool {
  return try {
    assert(x ?? false)
    true
  } catch {
    false
  }
}

pipeline default(harness: Harness) {
  for value in [true, false, nil] {
    harness.stdio.log(direct_accepts(value) == coalesced_accepts(value))
  }
}
",
    "[harn] true\n[harn] true\n[harn] true"
)]
#[case::source_assert_shadows_native_intrinsic(
    r#"
fn assert(value: bool?) -> bool { return true }

pipeline default(harness: Harness) {
  assert(nil ?? false)
  harness.stdio.log("continued")
}
"#,
    "[harn] continued"
)]
#[case::closure_binding_assert_shadows_native_intrinsic(
    r#"
pipeline default(harness: Harness) {
  const assert = { value -> if value == nil { "nil" } else { "false" } }
  harness.stdio.log(assert(nil ?? false))
}
"#,
    "[harn] false"
)]
#[case::test_logical_operators("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(true && false)\nharness.stdio.log(true || false)\nharness.stdio.log(!true) }", "[harn] false\n[harn] true\n[harn] false")]
#[case::test_subscript(
    r#"pipeline t(harness: Harness, task: unknown) {
const arr = [10, 20, 30]
const dict = {name: "harn"}
harness.stdio.log(arr[1])
harness.stdio.log(dict["name"])
harness.stdio.log("abc"[1])
harness.stdio.log("éx"[-1])
}"#,
    "[harn] 20\n[harn] harn\n[harn] b\n[harn] x"
)]
#[case::test_contains_is_the_membership_method_on_every_collection(
    r#"pipeline t(harness: Harness, task: unknown) {
harness.stdio.log("hello world".contains("world"))
harness.stdio.log(["alpha", "beta"].contains("beta"))
harness.stdio.log(set("read", "write").contains("write"))
harness.stdio.log((1..=4).contains(3))
}"#,
    "[harn] true\n[harn] true\n[harn] true\n[harn] true"
)]
#[case::test_string_length_fast_paths_keep_unicode_scalar_semantics(
    r#"pipeline t(harness: Harness, task: unknown) {
harness.stdio.log(len("abcd"))
harness.stdio.log(len("é🙂"))
harness.stdio.log("é🙂".count)
harness.stdio.log("é🙂".len())
}"#,
    "[harn] 4\n[harn] 2\n[harn] 2\n[harn] 2"
)]
#[case::test_recursive_function("pipeline t(harness: Harness, task: unknown) { fn fib(n) { if n <= 1 { return n }\nreturn fib(n - 1) + fib(n - 2) }\nharness.stdio.log(fib(10)) }", "[harn] 55")]
#[case::test_ternary(
    r#"pipeline t(harness: Harness, task: unknown) { const x = 5
const r = x > 0 ? "positive" : "non-positive"
harness.stdio.log(r) }"#,
    "[harn] positive"
)]
fn output_run_output(#[case] source: &str, #[case] expected: &str) {
    assert_eq!(run_output(source), expected);
}

#[test]
fn test_string_repeat_operator_rejects_oversized_count() {
    // `"a" * <huge>` must error cleanly, never OOM / panic `capacity overflow`.
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) { const s = "ab" * 9999999999 }"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_string_repeat_method_rejects_oversized_count() {
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) { const s = "ab".repeat(9999999999) }"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_str_pad_rejects_oversized_width() {
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) { const s = str_pad("x", 9999999999, "-") }"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}
