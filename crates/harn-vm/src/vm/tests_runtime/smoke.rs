//! End-to-end smoke corpus.
//!
//! One short program per language feature, each run through the full
//! compile-and-execute path, so a breakage anywhere in the pipeline surfaces as
//! a named failure rather than only inside a larger test.

use super::harness::*;
#[test]
fn test_hello_world() {
    let out = run_vm(
        r#"pipeline default(harness: Harness, task: unknown) { harness.stdio.log("hello") }"#,
    );
    assert_eq!(out, "[harn] hello\n");
}

#[test]
fn test_arithmetic_new() {
    let out =
        run_vm("pipeline default(harness: Harness, task: unknown) { harness.stdio.log(2 + 3) }");
    assert_eq!(out, "[harn] 5\n");
}

#[test]
fn test_string_concat_new() {
    let out = run_vm(
        r#"pipeline default(harness: Harness, task: unknown) { harness.stdio.log("a" + "b") }"#,
    );
    assert_eq!(out, "[harn] ab\n");
}

#[test]
fn test_if_else_new() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { if true { harness.stdio.log(1) } else { harness.stdio.log(2) } }");
    assert_eq!(out, "[harn] 1\n");
}

#[test]
fn test_for_loop_new() {
    let out = run_vm(
        "pipeline default(harness: Harness, task: unknown) { for i in [1, 2, 3] { harness.stdio.log(i) } }",
    );
    assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3\n");
}

#[test]
fn test_while_loop_new() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { let i = 0\nwhile i < 3 { harness.stdio.log(i)\ni = i + 1 } }");
    assert_eq!(out, "[harn] 0\n[harn] 1\n[harn] 2\n");
}

#[test]
fn test_function_call_new() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { fn add(a, b) { return a + b }\nharness.stdio.log(add(2, 3)) }");
    assert_eq!(out, "[harn] 5\n");
}

#[test]
fn test_closure_new() {
    let out =
        run_vm("pipeline default(harness: Harness, task: unknown) { const f = { x -> x * 2 }\nharness.stdio.log(f(5)) }");
    assert_eq!(out, "[harn] 10\n");
}

#[test]
fn test_recursion() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { fn fact(n) { if n <= 1 { return 1 }\nreturn n * fact(n - 1) }\nharness.stdio.log(fact(5)) }");
    assert_eq!(out, "[harn] 120\n");
}

#[test]
fn test_try_catch_new() {
    let out = run_vm(
        r#"pipeline default(harness: Harness, task: unknown) { try { throw "err" } catch (e) { harness.stdio.log(e) } }"#,
    );
    assert_eq!(out, "[harn] err\n");
}

#[test]
fn test_try_no_error_new() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { try { harness.stdio.log(1) } catch (e) { harness.stdio.log(2) } }");
    assert_eq!(out, "[harn] 1\n");
}

#[test]
fn test_list_map_new() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { const r = [1, 2, 3].map({ x -> x * 2 })\nharness.stdio.log(r) }");
    assert_eq!(out, "[harn] [2, 4, 6]\n");
}

#[test]
fn test_list_filter_new() {
    let out =
        run_vm("pipeline default(harness: Harness, task: unknown) { const r = [1, 2, 3, 4].filter({ x -> x > 2 })\nharness.stdio.log(r) }");
    assert_eq!(out, "[harn] [3, 4]\n");
}

#[test]
fn test_dict_access_new() {
    let out = run_vm(
        "pipeline default(harness: Harness, task: unknown) { const d = {name: \"Alice\"}\nharness.stdio.log(d.name) }",
    );
    assert_eq!(out, "[harn] Alice\n");
}

#[test]
fn test_string_interpolation() {
    let out =
        run_vm("pipeline default(harness: Harness, task: unknown) { const x = 42\nharness.stdio.log(\"val=${x}\") }");
    assert_eq!(out, "[harn] val=42\n");
}

#[test]
fn test_match_new() {
    let out = run_vm(
        "pipeline default(harness: Harness, task: unknown) { const x = \"b\"\nmatch x { \"a\" -> { harness.stdio.log(1) } \"b\" -> { harness.stdio.log(2) } } }",
    );
    assert_eq!(out, "[harn] 2\n");
}

#[test]
fn test_json_roundtrip() {
    let out = run_vm(
        "pipeline default(harness: Harness, task: unknown) { const s = json_stringify({a: 1})\nharness.stdio.log(s) }",
    );
    assert!(out.contains("\"a\""));
    assert!(out.contains('1'));
}

#[test]
fn test_type_of() {
    let out = run_vm("pipeline default(harness: Harness, task: unknown) { harness.stdio.log(type_of(42))\nharness.stdio.log(type_of(\"hi\")) }");
    assert_eq!(out, "[harn] int\n[harn] string\n");
}
