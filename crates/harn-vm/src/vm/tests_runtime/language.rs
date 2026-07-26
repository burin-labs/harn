//! Core language semantics executed end to end.
//!
//! Arithmetic and comparisons, control flow, functions and closures, the list
//! and dict builtins, string methods, list-pattern matching, the pipe operator,
//! nil coalescing, subscripting, and the guards on oversized string operations.

use super::harness::*;
#[test]
fn test_arithmetic() {
    let out = run_output("pipeline t(task) { log(2 + 3)\nlog(10 - 4)\nlog(3 * 5)\nlog(10 / 3) }");
    assert_eq!(out, "[harn] 5\n[harn] 6\n[harn] 15\n[harn] 3");
}

#[test]
fn test_mixed_arithmetic() {
    let out = run_output("pipeline t(task) { log(3 + 1.5)\nlog(10 - 2.5) }");
    assert_eq!(out, "[harn] 4.5\n[harn] 7.5");
}

#[test]
fn test_typed_opcode_drift_falls_back_to_generic() {
    // A value that drifts from its declared type no longer makes the typed
    // opcode hard-error with a specialization-internal message. The typed op
    // guards its operands and falls back to generic semantics — which here, for
    // genuinely incompatible `string + int`, still errors, but with the same
    // generic message the unoptimized build produces (so opt and unopt agree).
    // For a *compatible* drift (e.g. an `any` float into a declared int) the
    // fallback yields the correct coerced result instead of throwing; that is
    // covered in `vm::tests_typed_op_fallback`.
    let err = run_vm_err(
        r#"pipeline t(task) {
  const x: int = "bad"
  log(x + 1)
}"#,
    );
    assert!(
        err.contains("Cannot add") && err.contains("string"),
        "expected the generic add error, got: {err}"
    );
    assert!(
        !err.contains("Typed int"),
        "typed opcodes must no longer surface specialization-internal errors: {err}"
    );
}

#[test]
fn test_exponentiation() {
    let out = run_output(
        "pipeline t(task) { log(2 ** 8)\nlog(2 * 3 ** 2)\nlog(2 ** 3 ** 2)\nlog(2 ** -1) }",
    );
    assert_eq!(out, "[harn] 256\n[harn] 18\n[harn] 512\n[harn] 0.5");
}

#[test]
fn test_unary_minus_binds_looser_than_exponent() {
    // `-2 ** 2` is `-(2 ** 2) = -4`, matching Python/Ruby/math notation rather
    // than the spreadsheet `(-2) ** 2 = 4` reading. The exponent operand still
    // accepts a unary prefix, so `2 ** -2` stays `2 ** (-2)`.
    let out =
        run_output("pipeline t(task) { log(-2 ** 2)\nlog(-2 ** 3)\nlog(2 ** -2)\nlog(-(2 ** 2)) }");
    assert_eq!(out, "[harn] -4\n[harn] -8\n[harn] 0.25\n[harn] -4");
}

#[test]
fn test_comparisons() {
    let out = run_output("pipeline t(task) { log(1 < 2)\nlog(2 > 3)\nlog(1 == 1)\nlog(1 != 2) }");
    assert_eq!(out, "[harn] true\n[harn] false\n[harn] true\n[harn] true");
}

#[test]
fn test_let_var() {
    let out = run_output("pipeline t(task) { const x = 42\nlog(x)\nlet y = 1\ny = 2\nlog(y) }");
    assert_eq!(out, "[harn] 42\n[harn] 2");
}

#[test]
fn test_if_else() {
    let out = run_output(
        r#"pipeline t(task) { if true { log("yes") }
if false { log("wrong") } else { log("no") } }"#,
    );
    assert_eq!(out, "[harn] yes\n[harn] no");
}

#[test]
fn test_while_loop() {
    let out = run_output("pipeline t(task) { let i = 0\n while i < 5 { i = i + 1 }\n log(i) }");
    assert_eq!(out, "[harn] 5");
}

#[test]
fn test_for_in() {
    let out = run_output("pipeline t(task) { for item in [1, 2, 3] { log(item) } }");
    assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3");
}

#[test]
fn test_inner_for_return_does_not_leak_iterator_into_caller() {
    let out = run_output(
        r#"pipeline t(task) {
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
  log(join(seen, ","))
}"#,
    );
    assert_eq!(out, "[harn] outer:a");
}

#[test]
fn test_fn_decl_and_call() {
    let out = run_output("pipeline t(task) { fn add(a, b) { return a + b }\nlog(add(3, 4)) }");
    assert_eq!(out, "[harn] 7");
}

#[test]
fn test_closure() {
    let out = run_output("pipeline t(task) { const double = { x -> x * 2 }\nlog(double(5)) }");
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_closure_capture() {
    let out = run_output(
        "pipeline t(task) { const base = 10\nfn offset(x) { return x + base }\nlog(offset(5)) }",
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_string_concat() {
    let out = run_output(
        r#"pipeline t(task) { const a = "hello" + " " + "world"
log(a) }"#,
    );
    assert_eq!(out, "[harn] hello world");
}

#[test]
fn test_list_map() {
    let out = run_output(
        "pipeline t(task) { const doubled = [1, 2, 3].map({ x -> x * 2 })\nlog(doubled) }",
    );
    assert_eq!(out, "[harn] [2, 4, 6]");
}

#[test]
fn test_list_filter() {
    let out = run_output(
        "pipeline t(task) { const big = [1, 2, 3, 4, 5].filter({ x -> x > 3 })\nlog(big) }",
    );
    assert_eq!(out, "[harn] [4, 5]");
}

#[test]
fn test_list_reduce() {
    let out = run_output(
        "pipeline t(task) { const sum = [1, 2, 3, 4].reduce(0, { acc, x -> acc + x })\nlog(sum) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_dict_access() {
    let out = run_output(
        r#"pipeline t(task) { const d = {name: "test", value: 42}
log(d.name)
log(d.value) }"#,
    );
    assert_eq!(out, "[harn] test\n[harn] 42");
}

#[test]
fn test_dict_methods() {
    let out = run_output(
        r#"pipeline t(task) { const d = {a: 1, b: 2}
log(d.keys())
log(d.values())
log(d.has("a"))
log(d.has("z")) }"#,
    );
    assert_eq!(
        out,
        "[harn] [a, b]\n[harn] [1, 2]\n[harn] true\n[harn] false"
    );
}

#[test]
fn test_string_repeat_operator_rejects_oversized_count() {
    // `"a" * <huge>` must error cleanly, never OOM / panic `capacity overflow`.
    let result = run_harn_result(r#"pipeline t(task) { const s = "ab" * 9999999999 }"#);
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_string_repeat_method_rejects_oversized_count() {
    let result = run_harn_result(r#"pipeline t(task) { const s = "ab".repeat(9999999999) }"#);
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_str_pad_rejects_oversized_width() {
    let result = run_harn_result(r#"pipeline t(task) { const s = str_pad("x", 9999999999, "-") }"#);
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_string_repeat_operator_small_count_ok() {
    let out = run_output(r#"pipeline t(task) { log("ab" * 3) }"#);
    assert_eq!(out, "[harn] ababab");
}

#[test]
fn test_match_list_pattern_is_exact_length() {
    // A fixed-arity list pattern matches ONLY that length: `[a, b]` must NOT
    // match a 3-element list (previously it did via a `len >= 2` check).
    let out = run_output(
        r#"pipeline t(task) {
match [1, 2, 3] {
  [a, b] -> { log("two: ${a},${b}") }
  _ -> { log("other") }
} }"#,
    );
    assert_eq!(out, "[harn] other");
}

#[test]
fn test_match_list_pattern_exact_match_binds() {
    let out = run_output(
        r#"pipeline t(task) {
match [10, 20] {
  [a, b] -> { log("${a},${b}") }
  _ -> { log("other") }
} }"#,
    );
    assert_eq!(out, "[harn] 10,20");
}

#[test]
fn test_match_list_pattern_rest_binds_tail() {
    let out = run_output(
        r#"pipeline t(task) {
match [1, 2, 3] {
  [head, ...rest] -> { log("${head} :: ${rest}") }
  _ -> { log("none") }
} }"#,
    );
    assert_eq!(out, "[harn] 1 :: [2, 3]");
}

#[test]
fn test_match_list_pattern_rest_empty_tail() {
    let out = run_output(
        r#"pipeline t(task) {
match [42] {
  [head, ...rest] -> { log("${head} :: ${rest}") }
  _ -> { log("none") }
} }"#,
    );
    assert_eq!(out, "[harn] 42 :: []");
}

#[test]
fn test_match_list_pattern_discard_rest_matches_at_least() {
    let out = run_output(
        r#"pipeline t(task) {
match [1, 2, 3, 4] {
  [first, ..._] -> { log("first=${first}") }
  _ -> { log("empty") }
} }"#,
    );
    assert_eq!(out, "[harn] first=1");
}

#[test]
fn test_pipe_operator() {
    let out = run_output(
        "pipeline t(task) { fn double(x) { return x * 2 }\nlet r = 5 |> double\nlog(r) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_pipe_with_closure() {
    let out = run_output(
        r#"pipeline t(task) { const r = "hello world" |> { s -> s.split(" ") }
log(r) }"#,
    );
    assert_eq!(out, "[harn] [hello, world]");
}

#[test]
fn test_nil_coalescing() {
    let out = run_output(
        r#"pipeline t(task) { const a = nil ?? "fallback"
log(a)
const b = "present" ?? "fallback"
log(b) }"#,
    );
    assert_eq!(out, "[harn] fallback\n[harn] present");
}

#[test]
fn test_logical_operators() {
    let out = run_output("pipeline t(task) { log(true && false)\nlog(true || false)\nlog(!true) }");
    assert_eq!(out, "[harn] false\n[harn] true\n[harn] false");
}

#[test]
fn test_match() {
    let out = run_output(
        r#"pipeline t(task) { const x = "b"
match x { "a" -> { log("first") } "b" -> { log("second") } "c" -> { log("third") } } }"#,
    );
    assert_eq!(out, "[harn] second");
}

#[test]
fn test_subscript() {
    let out = run_output(
        r#"pipeline t(task) {
const arr = [10, 20, 30]
const dict = {name: "harn"}
log(arr[1])
log(dict["name"])
log("abc"[1])
log("éx"[-1])
}"#,
    );
    assert_eq!(out, "[harn] 20\n[harn] harn\n[harn] b\n[harn] x");
}

#[test]
fn test_string_methods() {
    let out = run_output(
        r#"pipeline t(task) { log("hello world".replace("world", "harn"))
log("a,b,c".split(","))
log("  hello  ".trim())
log("hello".starts_with("hel"))
log("hello".ends_with("lo"))
log("hello".substring(1, 3)) }"#,
    );
    assert_eq!(
        out,
        "[harn] hello harn\n[harn] [a, b, c]\n[harn] hello\n[harn] true\n[harn] true\n[harn] el"
    );
}

#[test]
fn test_includes_aliases_membership_methods() {
    let out = run_output(
        r#"pipeline t(task) {
log("hello world".includes("world"))
log(["alpha", "beta"].includes("beta"))
log(set("read", "write").includes("write"))
log((1 to 4).includes(3))
}"#,
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] true\n[harn] true");
}

#[test]
fn test_string_length_fast_paths_keep_unicode_scalar_semantics() {
    let out = run_output(
        r#"pipeline t(task) {
log(len("abcd"))
log(len("é🙂"))
log("é🙂".count)
log("é🙂".count())
log("é🙂".len())
}"#,
    );
    assert_eq!(out, "[harn] 4\n[harn] 2\n[harn] 2\n[harn] 2\n[harn] 2");
}

#[test]
fn test_list_properties() {
    let out = run_output(
        "pipeline t(task) { const list = [1, 2, 3]\nlog(list.count)\nlog(list.empty)\nlog(list.first)\nlog(list.last) }",
    );
    assert_eq!(out, "[harn] 3\n[harn] false\n[harn] 1\n[harn] 3");
}

#[test]
fn test_recursive_function() {
    let out = run_output(
        "pipeline t(task) { fn fib(n) { if n <= 1 { return n }\nreturn fib(n - 1) + fib(n - 2) }\nlog(fib(10)) }",
    );
    assert_eq!(out, "[harn] 55");
}

#[test]
fn test_ternary() {
    let out = run_output(
        r#"pipeline t(task) { const x = 5
const r = x > 0 ? "positive" : "non-positive"
log(r) }"#,
    );
    assert_eq!(out, "[harn] positive");
}

#[test]
fn test_for_in_dict() {
    let out = run_output(
        "pipeline t(task) { const d = {a: 1, b: 2}\nfor entry in d { log(entry.key) } }",
    );
    assert_eq!(out, "[harn] a\n[harn] b");
}

#[test]
fn test_list_any_all() {
    let out = run_output(
        "pipeline t(task) { const nums = [2, 4, 6]\nlog(nums.any({ x -> x > 5 }))\nlog(nums.all({ x -> x > 0 }))\nlog(nums.all({ x -> x > 3 })) }",
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] false");
}
