//! Core language semantics executed end to end.
//!
//! Arithmetic and comparisons, control flow, functions and closures, the list
//! and dict builtins, string methods, list-pattern matching, the pipe operator,
//! nil coalescing, subscripting, and the guards on oversized string operations.

use super::harness::*;
#[test]
fn test_arithmetic() {
    let out = run_output("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(2 + 3)\nharness.stdio.log(10 - 4)\nharness.stdio.log(3 * 5)\nharness.stdio.log(10 / 3) }");
    assert_eq!(out, "[harn] 5\n[harn] 6\n[harn] 15\n[harn] 3");
}

#[test]
fn test_mixed_arithmetic() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { harness.stdio.log(3 + 1.5)\nharness.stdio.log(10 - 2.5) }",
    );
    assert_eq!(out, "[harn] 4.5\n[harn] 7.5");
}

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

#[test]
fn test_exponentiation() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { harness.stdio.log(2 ** 8)\nharness.stdio.log(2 * 3 ** 2)\nharness.stdio.log(2 ** 3 ** 2)\nharness.stdio.log(2 ** -1) }",
    );
    assert_eq!(out, "[harn] 256\n[harn] 18\n[harn] 512\n[harn] 0.5");
}

#[test]
fn test_unary_minus_binds_looser_than_exponent() {
    // `-2 ** 2` is `-(2 ** 2) = -4`, matching Python/Ruby/math notation rather
    // than the spreadsheet `(-2) ** 2 = 4` reading. The exponent operand still
    // accepts a unary prefix, so `2 ** -2` stays `2 ** (-2)`.
    let out =
        run_output("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(-2 ** 2)\nharness.stdio.log(-2 ** 3)\nharness.stdio.log(2 ** -2)\nharness.stdio.log(-(2 ** 2)) }");
    assert_eq!(out, "[harn] -4\n[harn] -8\n[harn] 0.25\n[harn] -4");
}

#[test]
fn test_comparisons() {
    let out = run_output("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(1 < 2)\nharness.stdio.log(2 > 3)\nharness.stdio.log(1 == 1)\nharness.stdio.log(1 != 2) }");
    assert_eq!(out, "[harn] true\n[harn] false\n[harn] true\n[harn] true");
}

#[test]
fn test_let_var() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const x = 42\nharness.stdio.log(x)\nlet y = 1\ny = 2\nharness.stdio.log(y) }",
    );
    assert_eq!(out, "[harn] 42\n[harn] 2");
}

#[test]
fn test_if_else() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { if true { harness.stdio.log("yes") }
if false { harness.stdio.log("wrong") } else { harness.stdio.log("no") } }"#,
    );
    assert_eq!(out, "[harn] yes\n[harn] no");
}

#[test]
fn test_while_loop() {
    let out = run_output("pipeline t(harness: Harness, task: unknown) { let i = 0\n while i < 5 { i = i + 1 }\n harness.stdio.log(i) }");
    assert_eq!(out, "[harn] 5");
}

#[test]
fn test_for_in() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { for item in [1, 2, 3] { harness.stdio.log(item) } }",
    );
    assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3");
}

#[test]
fn test_inner_for_return_does_not_leak_iterator_into_caller() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
  fn first_match() {
    for pattern in ["a", "b"] {
      return pattern
    }
    return ""
  }

  let seen = []
  for path in ["outer"] {
    seen = seen + [path + ":" + first_match()]
  }
  harness.stdio.log(join(seen, ","))
}"#,
    );
    assert_eq!(out, "[harn] outer:a");
}

#[test]
fn test_fn_decl_and_call() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { fn add(a, b) { return a + b }\nharness.stdio.log(add(3, 4)) }",
    );
    assert_eq!(out, "[harn] 7");
}

#[test]
fn test_closure() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const double = { x -> x * 2 }\nharness.stdio.log(double(5)) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_closure_capture() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const base = 10\nfn offset(x) { return x + base }\nharness.stdio.log(offset(5)) }",
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_string_concat() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const a = "hello" + " " + "world"
harness.stdio.log(a) }"#,
    );
    assert_eq!(out, "[harn] hello world");
}

#[test]
fn test_list_map() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const doubled = [1, 2, 3].map({ x -> x * 2 })\nharness.stdio.log(doubled) }",
    );
    assert_eq!(out, "[harn] [2, 4, 6]");
}

#[test]
fn test_list_filter() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const big = [1, 2, 3, 4, 5].filter({ x -> x > 3 })\nharness.stdio.log(big) }",
    );
    assert_eq!(out, "[harn] [4, 5]");
}

#[test]
fn test_list_reduce() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const sum = [1, 2, 3, 4].reduce(0, { acc, x -> acc + x })\nharness.stdio.log(sum) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_dict_access() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const d = {name: "test", value: 42}
harness.stdio.log(d.name)
harness.stdio.log(d.value) }"#,
    );
    assert_eq!(out, "[harn] test\n[harn] 42");
}

#[test]
fn test_dict_methods() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const d = {a: 1, b: 2}
harness.stdio.log(d.keys())
harness.stdio.log(d.values())
harness.stdio.log(d.has("a"))
harness.stdio.log(d.has("z")) }"#,
    );
    assert_eq!(
        out,
        "[harn] [a, b]\n[harn] [1, 2]\n[harn] true\n[harn] false"
    );
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

#[test]
fn test_string_repeat_operator_small_count_ok() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { harness.stdio.log("ab" * 3) }"#,
    );
    assert_eq!(out, "[harn] ababab");
}

#[test]
fn test_match_list_pattern_is_exact_length() {
    // A fixed-arity list pattern matches ONLY that length: `[a, b]` must NOT
    // match a 3-element list (previously it did via a `len >= 2` check).
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3] {
  [a, b] -> { harness.stdio.log("two: ${a},${b}") }
  _ -> { harness.stdio.log("other") }
} }"#,
    );
    assert_eq!(out, "[harn] other");
}

#[test]
fn test_match_list_pattern_exact_match_binds() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
match [10, 20] {
  [a, b] -> { harness.stdio.log("${a},${b}") }
  _ -> { harness.stdio.log("other") }
} }"#,
    );
    assert_eq!(out, "[harn] 10,20");
}

#[test]
fn test_match_list_pattern_rest_binds_tail() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3] {
  [head, ...rest] -> { harness.stdio.log("${head} :: ${rest}") }
  _ -> { harness.stdio.log("none") }
} }"#,
    );
    assert_eq!(out, "[harn] 1 :: [2, 3]");
}

#[test]
fn test_match_list_pattern_rest_empty_tail() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
match [42] {
  [head, ...rest] -> { harness.stdio.log("${head} :: ${rest}") }
  _ -> { harness.stdio.log("none") }
} }"#,
    );
    assert_eq!(out, "[harn] 42 :: []");
}

#[test]
fn test_match_list_pattern_discard_rest_matches_at_least() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
match [1, 2, 3, 4] {
  [first, ..._] -> { harness.stdio.log("first=${first}") }
  _ -> { harness.stdio.log("empty") }
} }"#,
    );
    assert_eq!(out, "[harn] first=1");
}

#[test]
fn test_pipe_operator() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { fn double(x) { return x * 2 }\nlet r = 5 |> double\nharness.stdio.log(r) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_pipe_with_closure() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const r = "hello world" |> { s -> s.split(" ") }
harness.stdio.log(r) }"#,
    );
    assert_eq!(out, "[harn] [hello, world]");
}

#[test]
fn test_nil_coalescing() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const a = nil ?? "fallback"
harness.stdio.log(a)
const b = "present" ?? "fallback"
harness.stdio.log(b) }"#,
    );
    assert_eq!(out, "[harn] fallback\n[harn] present");
}

#[test]
fn positive_assert_false_fallback_matches_native_truthiness() {
    let out = run_output(
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
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] true");
}

#[test]
fn test_logical_operators() {
    let out = run_output("pipeline t(harness: Harness, task: unknown) { harness.stdio.log(true && false)\nharness.stdio.log(true || false)\nharness.stdio.log(!true) }");
    assert_eq!(out, "[harn] false\n[harn] true\n[harn] false");
}

#[test]
fn test_match() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const x = "b"
match x { "a" -> { harness.stdio.log("first") } "b" -> { harness.stdio.log("second") } "c" -> { harness.stdio.log("third") } } }"#,
    );
    assert_eq!(out, "[harn] second");
}

#[test]
fn test_subscript() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
const arr = [10, 20, 30]
const dict = {name: "harn"}
harness.stdio.log(arr[1])
harness.stdio.log(dict["name"])
harness.stdio.log("abc"[1])
harness.stdio.log("éx"[-1])
}"#,
    );
    assert_eq!(out, "[harn] 20\n[harn] harn\n[harn] b\n[harn] x");
}

#[test]
fn test_string_methods() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { harness.stdio.log("hello world".replace("world", "harn"))
harness.stdio.log("a,b,c".split(","))
harness.stdio.log("  hello  ".trim())
harness.stdio.log("hello".starts_with("hel"))
harness.stdio.log("hello".ends_with("lo"))
harness.stdio.log("hello".substring(1, 3)) }"#,
    );
    assert_eq!(
        out,
        "[harn] hello harn\n[harn] [a, b, c]\n[harn] hello\n[harn] true\n[harn] true\n[harn] el"
    );
}

#[test]
fn test_includes_aliases_membership_methods() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
harness.stdio.log("hello world".includes("world"))
harness.stdio.log(["alpha", "beta"].includes("beta"))
harness.stdio.log(set("read", "write").includes("write"))
harness.stdio.log((1 to 4).includes(3))
}"#,
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] true\n[harn] true");
}

#[test]
fn test_string_length_fast_paths_keep_unicode_scalar_semantics() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
harness.stdio.log(len("abcd"))
harness.stdio.log(len("é🙂"))
harness.stdio.log("é🙂".count)
harness.stdio.log("é🙂".count())
harness.stdio.log("é🙂".len())
}"#,
    );
    assert_eq!(out, "[harn] 4\n[harn] 2\n[harn] 2\n[harn] 2\n[harn] 2");
}

#[test]
fn test_list_properties() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const list = [1, 2, 3]\nharness.stdio.log(list.count)\nharness.stdio.log(list.empty)\nharness.stdio.log(list.first)\nharness.stdio.log(list.last) }",
    );
    assert_eq!(out, "[harn] 3\n[harn] false\n[harn] 1\n[harn] 3");
}

#[test]
fn test_recursive_function() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { fn fib(n) { if n <= 1 { return n }\nreturn fib(n - 1) + fib(n - 2) }\nharness.stdio.log(fib(10)) }",
    );
    assert_eq!(out, "[harn] 55");
}

#[test]
fn test_ternary() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { const x = 5
const r = x > 0 ? "positive" : "non-positive"
harness.stdio.log(r) }"#,
    );
    assert_eq!(out, "[harn] positive");
}

#[test]
fn test_for_in_dict() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const d = {a: 1, b: 2}\nfor entry in d { harness.stdio.log(entry.key) } }",
    );
    assert_eq!(out, "[harn] a\n[harn] b");
}

#[test]
fn test_list_any_all() {
    let out = run_output(
        "pipeline t(harness: Harness, task: unknown) { const nums = [2, 4, 6]\nharness.stdio.log(nums.any({ x -> x > 5 }))\nharness.stdio.log(nums.all({ x -> x > 0 }))\nharness.stdio.log(nums.all({ x -> x > 3 })) }",
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] false");
}
