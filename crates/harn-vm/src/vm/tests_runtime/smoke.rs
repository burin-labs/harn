//! End-to-end smoke corpus.
//!
//! One short program per language feature, each run through the full
//! compile-and-execute path, so a breakage anywhere in the pipeline surfaces as
//! a named failure rather than only inside a larger test.

use super::harness::*;
#[test]
fn test_hello_world() {
    let out = run_vm(r#"pipeline default(task) { log("hello") }"#);
    assert_eq!(out, "[harn] hello\n");
}

#[test]
fn test_arithmetic_new() {
    let out = run_vm("pipeline default(task) { log(2 + 3) }");
    assert_eq!(out, "[harn] 5\n");
}

#[test]
fn test_string_concat_new() {
    let out = run_vm(r#"pipeline default(task) { log("a" + "b") }"#);
    assert_eq!(out, "[harn] ab\n");
}

#[test]
fn test_if_else_new() {
    let out = run_vm("pipeline default(task) { if true { log(1) } else { log(2) } }");
    assert_eq!(out, "[harn] 1\n");
}

#[test]
fn test_for_loop_new() {
    let out = run_vm("pipeline default(task) { for i in [1, 2, 3] { log(i) } }");
    assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3\n");
}

#[test]
fn test_while_loop_new() {
    let out = run_vm("pipeline default(task) { let i = 0\nwhile i < 3 { log(i)\ni = i + 1 } }");
    assert_eq!(out, "[harn] 0\n[harn] 1\n[harn] 2\n");
}

#[test]
fn test_function_call_new() {
    let out = run_vm("pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(2, 3)) }");
    assert_eq!(out, "[harn] 5\n");
}

#[test]
fn test_closure_new() {
    let out = run_vm("pipeline default(task) { const f = { x -> x * 2 }\nlog(f(5)) }");
    assert_eq!(out, "[harn] 10\n");
}

#[test]
fn test_recursion() {
    let out = run_vm("pipeline default(task) { fn fact(n) { if n <= 1 { return 1 }\nreturn n * fact(n - 1) }\nlog(fact(5)) }");
    assert_eq!(out, "[harn] 120\n");
}

#[test]
fn test_try_catch_new() {
    let out = run_vm(r#"pipeline default(task) { try { throw "err" } catch (e) { log(e) } }"#);
    assert_eq!(out, "[harn] err\n");
}

#[test]
fn test_try_no_error_new() {
    let out = run_vm("pipeline default(task) { try { log(1) } catch (e) { log(2) } }");
    assert_eq!(out, "[harn] 1\n");
}

#[test]
fn test_list_map_new() {
    let out = run_vm("pipeline default(task) { const r = [1, 2, 3].map({ x -> x * 2 })\nlog(r) }");
    assert_eq!(out, "[harn] [2, 4, 6]\n");
}

#[test]
fn test_list_filter_new() {
    let out =
        run_vm("pipeline default(task) { const r = [1, 2, 3, 4].filter({ x -> x > 2 })\nlog(r) }");
    assert_eq!(out, "[harn] [3, 4]\n");
}

#[test]
fn test_dict_access_new() {
    let out = run_vm("pipeline default(task) { const d = {name: \"Alice\"}\nlog(d.name) }");
    assert_eq!(out, "[harn] Alice\n");
}

#[test]
fn test_string_interpolation() {
    let out = run_vm("pipeline default(task) { const x = 42\nlog(\"val=${x}\") }");
    assert_eq!(out, "[harn] val=42\n");
}

#[test]
fn test_match_new() {
    let out = run_vm(
        "pipeline default(task) { const x = \"b\"\nmatch x { \"a\" -> { log(1) } \"b\" -> { log(2) } } }",
    );
    assert_eq!(out, "[harn] 2\n");
}

#[test]
fn test_json_roundtrip() {
    let out = run_vm("pipeline default(task) { const s = json_stringify({a: 1})\nlog(s) }");
    assert!(out.contains("\"a\""));
    assert!(out.contains('1'));
}

#[test]
fn test_type_of() {
    let out = run_vm("pipeline default(task) { log(type_of(42))\nlog(type_of(\"hi\")) }");
    assert_eq!(out, "[harn] int\n[harn] string\n");
}
