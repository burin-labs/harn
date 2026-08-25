//! Raising, catching, and rendering runtime errors.
//!
//! `try`/`catch`/`throw` including nesting and rebinding the catch name, the
//! argument type-mismatch rejections for user and builtin calls, the arithmetic
//! edge cases (stack overflow, integer and float division by zero, promotion on
//! overflow), and frame-path normalization in the error renderer.

use crate::{VmError, VmValue};

use super::harness::*;
use crate::vm::*;
#[test]
fn runtime_error_renderer_normalizes_frame_paths() {
    let mut vm = Vm::new();
    vm.set_source_info(
        "/workspace/pipelines/mode/../mode/auto.harn",
        "const run = agent_loop(message, system_prompt, opts)\n",
    );
    vm.error_stack_trace = vec![
        (
            "pipeline".to_string(),
            39,
            1,
            Some("/workspace/pipelines/mode/../mode/auto.harn".to_string()),
        ),
        (
            "agent_loop".to_string(),
            205,
            48,
            Some("/workspace/pipelines/mode/../lib/runtime/loop.harn".to_string()),
        ),
    ];

    let rendered = vm.format_runtime_error(&VmError::Runtime(
        "option `cache` is not supported".to_string(),
    ));

    assert!(rendered.contains("--> /workspace/pipelines/lib/runtime/loop.harn:205:48"));
    assert!(rendered.contains("called from pipeline at /workspace/pipelines/mode/auto.harn:39"));
    assert!(!rendered.contains("/../"));
}

#[test]
fn test_try_catch_basic() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) { try { throw "oops" } catch(e) { harness.stdio.log("caught: " + e) } }"#,
    );
    assert_eq!(out, "[harn] caught: oops");
}

#[test]
fn test_try_no_error() {
    let out = run_output(
        r"pipeline t(harness: Harness, task: unknown) {
let result = 0
try { result = 42 } catch(e) { result = 0 }
harness.stdio.log(result)
}",
    );
    assert_eq!(out, "[harn] 42");
}

#[test]
fn test_throw_uncaught() {
    let result = run_harn_result(r#"pipeline t(harness: Harness, task: unknown) { throw "boom" }"#);
    assert!(result.is_err());
}

#[test]
fn test_runtime_user_call_arg_type_mismatch() {
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) {
fn add_one(value: int) -> int { return value + 1 }
add_one("bad")
}"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("parameter 'value' expected int"), "{err}");
    assert!(err.contains("got string"), "{err}");
}

#[test]
fn test_runtime_user_call_rest_arg_type_mismatch() {
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) {
fn collect(...values: int) -> int { return values.count() }
collect(1, "bad")
}"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("parameter 'values' expected int"), "{err}");
    assert!(err.contains("got string"), "{err}");
}

#[test]
fn test_runtime_user_call_named_struct_type_mismatch() {
    let result = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) {
struct Point { x: int }
struct User { name: string }
fn x_of(point: Point) -> int { return point.x }
x_of(User({name: "Ada"}))
}"#,
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("parameter `point` expects Point"), "{err}");
    assert!(err.contains("got struct"), "{err}");
}

#[test]
fn test_runtime_user_call_generic_param_is_static_only() {
    let (_, result) = run_harn_result(
        r#"pipeline t(harness: Harness, task: unknown) {
fn first<T>(xs: list<T>) -> T { return xs[0] }
return first(["ok"])
}"#,
    )
    .unwrap();
    assert!(matches!(result, VmValue::String(s) if s.as_str() == "ok"));
}

#[test]
fn test_runtime_user_call_missing_required_arg_rejected() {
    let result = run_harn_result(
        r"pipeline t(harness: Harness, task: unknown) {
fn echo(value) { return value }
echo()
}",
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Arity mismatch: 'echo'"), "{err}");
    assert!(err.contains("expects at least 1 argument, got 0"), "{err}");
}

#[test]
fn test_runtime_builtin_call_arg_type_mismatch() {
    let result = run_harn_result(r"pipeline t(harness: Harness, task: unknown) { lowercase(42) }");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("'lowercase' parameter `text` expects string"),
        "{err}"
    );
    assert!(err.contains("got int"), "{err}");
}

// --- Additional test coverage ---

#[test]
fn test_stack_overflow() {
    let err =
        run_vm_err("pipeline default(harness: Harness, task: unknown) { fn f() { f() }\nf() }");
    assert!(
        err.contains("stack") || err.contains("overflow") || err.contains("recursion"),
        "Expected stack overflow error, got: {err}"
    );
}

#[test]
fn test_division_by_zero() {
    let err = run_vm_err(
        "pipeline default(harness: Harness, task: unknown) { harness.stdio.log(1 / 0) }",
    );
    assert!(
        err.contains("Division by zero") || err.contains("division"),
        "Expected division by zero error, got: {err}"
    );
}

#[test]
fn test_int_division_overflow_promotes_instead_of_wrapping() {
    let out = run_output(
        r"pipeline default(harness: Harness, task: unknown) {
  const min = -9223372036854775807 - 1
  harness.stdio.log(min / -1)
  harness.stdio.log(min % -1)
}",
    );
    // `min / -1` overflows i64 (true value `i64::MAX + 1`); the VM promotes to
    // float rather than wrapping back to `i64::MIN`, matching `+`/`-`/`*`/neg.
    // `min % -1` is 0, the mathematically correct remainder.
    assert_eq!(out, "[harn] 9223372036854776000\n[harn] 0");
}

#[test]
fn test_float_division_by_zero_uses_ieee_values() {
    let out = run_vm(
        "pipeline default(harness: Harness, task: unknown) { harness.stdio.log(is_nan(0.0 / 0.0))\nharness.stdio.log(is_infinite(1.0 / 0.0))\nharness.stdio.log(is_infinite(-1.0 / 0.0)) }",
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] true\n");
}

#[test]
fn test_reusing_catch_binding_name_in_same_block() {
    let out = run_vm(
        r#"pipeline default(harness: Harness, task: unknown) {
try {
    throw "a"
} catch e {
    harness.stdio.log(e)
}
try {
    throw "b"
} catch e {
    harness.stdio.log(e)
}
}"#,
    );
    assert_eq!(out, "[harn] a\n[harn] b\n");
}

#[test]
fn test_try_catch_nested() {
    let out = run_output(
        r#"pipeline t(harness: Harness, task: unknown) {
try {
    try {
        throw "inner"
    } catch(e) {
        harness.stdio.log("inner caught: " + e)
        throw "outer"
    }
} catch(e2) {
    harness.stdio.log("outer caught: " + e2)
}
}"#,
    );
    assert_eq!(
        out,
        "[harn] inner caught: inner\n[harn] outer caught: outer"
    );
}

// --- Concurrency tests ---
