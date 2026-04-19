use std::collections::HashSet;

use crate::compiler::Compiler;
use crate::stdlib::register_vm_stdlib;
use crate::{VmError, VmValue};
use harn_lexer::Lexer;
use harn_parser::Parser;

use super::*;

fn run_harn(source: &str) -> (String, VmValue) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm.execute(&chunk).await.unwrap();
                (vm.output().to_string(), result)
            })
            .await
    })
}

fn run_output(source: &str) -> String {
    run_harn(source).0.trim_end().to_string()
}

fn run_harn_result(source: &str) -> Result<(String, VmValue), VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}

fn run_vm(source: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.unwrap();
                vm.output().to_string()
            })
            .await
    })
}

fn run_vm_err(source: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                match vm.execute(&chunk).await {
                    Err(e) => format!("{}", e),
                    Ok(_) => panic!("Expected error"),
                }
            })
            .await
    })
}

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
fn test_exponentiation() {
    let out = run_output(
        "pipeline t(task) { log(2 ** 8)\nlog(2 * 3 ** 2)\nlog(2 ** 3 ** 2)\nlog(2 ** -1) }",
    );
    assert_eq!(out, "[harn] 256\n[harn] 18\n[harn] 512\n[harn] 0.5");
}

#[test]
fn test_comparisons() {
    let out = run_output("pipeline t(task) { log(1 < 2)\nlog(2 > 3)\nlog(1 == 1)\nlog(1 != 2) }");
    assert_eq!(out, "[harn] true\n[harn] false\n[harn] true\n[harn] true");
}

#[test]
fn test_let_var() {
    let out = run_output("pipeline t(task) { let x = 42\nlog(x)\nvar y = 1\ny = 2\nlog(y) }");
    assert_eq!(out, "[harn] 42\n[harn] 2");
}

#[test]
fn test_if_else() {
    let out = run_output(
        r#"pipeline t(task) { if true { log("yes") } if false { log("wrong") } else { log("no") } }"#,
    );
    assert_eq!(out, "[harn] yes\n[harn] no");
}

#[test]
fn test_while_loop() {
    let out = run_output("pipeline t(task) { var i = 0\n while i < 5 { i = i + 1 }\n log(i) }");
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

  var seen = []
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
    let out = run_output("pipeline t(task) { let double = { x -> x * 2 }\nlog(double(5)) }");
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_closure_capture() {
    let out = run_output(
        "pipeline t(task) { let base = 10\nfn offset(x) { return x + base }\nlog(offset(5)) }",
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_string_concat() {
    let out = run_output(
        r#"pipeline t(task) { let a = "hello" + " " + "world"
log(a) }"#,
    );
    assert_eq!(out, "[harn] hello world");
}

#[test]
fn test_list_map() {
    let out = run_output(
        "pipeline t(task) { let doubled = [1, 2, 3].map({ x -> x * 2 })\nlog(doubled) }",
    );
    assert_eq!(out, "[harn] [2, 4, 6]");
}

#[test]
fn test_list_filter() {
    let out = run_output(
        "pipeline t(task) { let big = [1, 2, 3, 4, 5].filter({ x -> x > 3 })\nlog(big) }",
    );
    assert_eq!(out, "[harn] [4, 5]");
}

#[test]
fn test_list_reduce() {
    let out = run_output(
        "pipeline t(task) { let sum = [1, 2, 3, 4].reduce(0, { acc, x -> acc + x })\nlog(sum) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_dict_access() {
    let out = run_output(
        r#"pipeline t(task) { let d = {name: "test", value: 42}
log(d.name)
log(d.value) }"#,
    );
    assert_eq!(out, "[harn] test\n[harn] 42");
}

#[test]
fn test_dict_methods() {
    let out = run_output(
        r#"pipeline t(task) { let d = {a: 1, b: 2}
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
fn test_pipe_operator() {
    let out = run_output(
        "pipeline t(task) { fn double(x) { return x * 2 }\nlet r = 5 |> double\nlog(r) }",
    );
    assert_eq!(out, "[harn] 10");
}

#[test]
fn test_pipe_with_closure() {
    let out = run_output(
        r#"pipeline t(task) { let r = "hello world" |> { s -> s.split(" ") }
log(r) }"#,
    );
    assert_eq!(out, "[harn] [hello, world]");
}

#[test]
fn test_nil_coalescing() {
    let out = run_output(
        r#"pipeline t(task) { let a = nil ?? "fallback"
log(a)
let b = "present" ?? "fallback"
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
        r#"pipeline t(task) { let x = "b"
match x { "a" -> { log("first") } "b" -> { log("second") } "c" -> { log("third") } } }"#,
    );
    assert_eq!(out, "[harn] second");
}

#[test]
fn test_subscript() {
    let out = run_output("pipeline t(task) { let arr = [10, 20, 30]\nlog(arr[1]) }");
    assert_eq!(out, "[harn] 20");
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
fn test_list_properties() {
    let out = run_output(
        "pipeline t(task) { let list = [1, 2, 3]\nlog(list.count)\nlog(list.empty)\nlog(list.first)\nlog(list.last) }",
    );
    assert_eq!(out, "[harn] 3\n[harn] false\n[harn] 1\n[harn] 3");
}

#[test]
fn test_recursive_function() {
    let out = run_output(
        "pipeline t(task) { fn fib(n) { if n <= 1 { return n } return fib(n - 1) + fib(n - 2) }\nlog(fib(10)) }",
    );
    assert_eq!(out, "[harn] 55");
}

#[test]
fn test_ternary() {
    let out = run_output(
        r#"pipeline t(task) { let x = 5
let r = x > 0 ? "positive" : "non-positive"
log(r) }"#,
    );
    assert_eq!(out, "[harn] positive");
}

#[test]
fn test_for_in_dict() {
    let out =
        run_output("pipeline t(task) { let d = {a: 1, b: 2}\nfor entry in d { log(entry.key) } }");
    assert_eq!(out, "[harn] a\n[harn] b");
}

#[test]
fn test_list_any_all() {
    let out = run_output(
        "pipeline t(task) { let nums = [2, 4, 6]\nlog(nums.any({ x -> x > 5 }))\nlog(nums.all({ x -> x > 0 }))\nlog(nums.all({ x -> x > 3 })) }",
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] false");
}

#[test]
fn test_disassembly() {
    let mut lexer = Lexer::new("pipeline t(task) { log(2 + 3) }");
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let chunk = Compiler::new().compile(&program).unwrap();
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CONSTANT"));
    assert!(disasm.contains("ADD"));
    assert!(disasm.contains("CALL"));
}

// --- Error handling tests ---

#[test]
fn test_try_catch_basic() {
    let out =
        run_output(r#"pipeline t(task) { try { throw "oops" } catch(e) { log("caught: " + e) } }"#);
    assert_eq!(out, "[harn] caught: oops");
}

#[test]
fn test_try_no_error() {
    let out = run_output(
        r#"pipeline t(task) {
var result = 0
try { result = 42 } catch(e) { result = 0 }
log(result)
}"#,
    );
    assert_eq!(out, "[harn] 42");
}

#[test]
fn test_throw_uncaught() {
    let result = run_harn_result(r#"pipeline t(task) { throw "boom" }"#);
    assert!(result.is_err());
}

// --- Additional test coverage ---

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
    let out = run_vm("pipeline default(task) { var i = 0\nwhile i < 3 { log(i)\ni = i + 1 } }");
    assert_eq!(out, "[harn] 0\n[harn] 1\n[harn] 2\n");
}

#[test]
fn test_function_call_new() {
    let out = run_vm("pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(2, 3)) }");
    assert_eq!(out, "[harn] 5\n");
}

#[test]
fn test_closure_new() {
    let out = run_vm("pipeline default(task) { let f = { x -> x * 2 }\nlog(f(5)) }");
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
    let out = run_vm("pipeline default(task) { let r = [1, 2, 3].map({ x -> x * 2 })\nlog(r) }");
    assert_eq!(out, "[harn] [2, 4, 6]\n");
}

#[test]
fn test_list_filter_new() {
    let out =
        run_vm("pipeline default(task) { let r = [1, 2, 3, 4].filter({ x -> x > 2 })\nlog(r) }");
    assert_eq!(out, "[harn] [3, 4]\n");
}

#[test]
fn test_dict_access_new() {
    let out = run_vm("pipeline default(task) { let d = {name: \"Alice\"}\nlog(d.name) }");
    assert_eq!(out, "[harn] Alice\n");
}

#[test]
fn test_string_interpolation() {
    let out = run_vm("pipeline default(task) { let x = 42\nlog(\"val=${x}\") }");
    assert_eq!(out, "[harn] val=42\n");
}

#[test]
fn test_match_new() {
    let out = run_vm(
        "pipeline default(task) { let x = \"b\"\nmatch x { \"a\" -> { log(1) } \"b\" -> { log(2) } } }",
    );
    assert_eq!(out, "[harn] 2\n");
}

#[test]
fn test_json_roundtrip() {
    let out = run_vm("pipeline default(task) { let s = json_stringify({a: 1})\nlog(s) }");
    assert!(out.contains("\"a\""));
    assert!(out.contains("1"));
}

#[test]
fn test_type_of() {
    let out = run_vm("pipeline default(task) { log(type_of(42))\nlog(type_of(\"hi\")) }");
    assert_eq!(out, "[harn] int\n[harn] string\n");
}

#[test]
fn test_stack_overflow() {
    let err = run_vm_err("pipeline default(task) { fn f() { f() }\nf() }");
    assert!(
        err.contains("stack") || err.contains("overflow") || err.contains("recursion"),
        "Expected stack overflow error, got: {}",
        err
    );
}

#[test]
fn test_division_by_zero() {
    let err = run_vm_err("pipeline default(task) { log(1 / 0) }");
    assert!(
        err.contains("Division by zero") || err.contains("division"),
        "Expected division by zero error, got: {}",
        err
    );
}

#[test]
fn test_float_division_by_zero_uses_ieee_values() {
    let out = run_vm(
        "pipeline default(task) { log(is_nan(0.0 / 0.0))\nlog(is_infinite(1.0 / 0.0))\nlog(is_infinite(-1.0 / 0.0)) }",
    );
    assert_eq!(out, "[harn] true\n[harn] true\n[harn] true\n");
}

#[test]
fn test_reusing_catch_binding_name_in_same_block() {
    let out = run_vm(
        r#"pipeline default(task) {
try {
    throw "a"
} catch e {
    log(e)
}
try {
    throw "b"
} catch e {
    log(e)
}
}"#,
    );
    assert_eq!(out, "[harn] a\n[harn] b\n");
}

#[test]
fn test_try_catch_nested() {
    let out = run_output(
        r#"pipeline t(task) {
try {
    try {
        throw "inner"
    } catch(e) {
        log("inner caught: " + e)
        throw "outer"
    }
} catch(e2) {
    log("outer caught: " + e2)
}
}"#,
    );
    assert_eq!(
        out,
        "[harn] inner caught: inner\n[harn] outer caught: outer"
    );
}

// --- Concurrency tests ---

#[test]
fn test_parallel_basic() {
    let out =
        run_output("pipeline t(task) { let results = parallel(3) { i -> i * 10 }\nlog(results) }");
    assert_eq!(out, "[harn] [0, 10, 20]");
}

#[test]
fn test_parallel_no_variable() {
    let out = run_output("pipeline t(task) { let results = parallel(3) { 42 }\nlog(results) }");
    assert_eq!(out, "[harn] [42, 42, 42]");
}

#[test]
fn test_parallel_each_basic() {
    let out = run_output(
        "pipeline t(task) { let results = parallel each [1, 2, 3] { x -> x * x }\nlog(results) }",
    );
    assert_eq!(out, "[harn] [1, 4, 9]");
}

#[test]
fn test_spawn_await() {
    let out = run_output(
        r#"pipeline t(task) {
let handle = spawn { log("spawned") }
let result = await(handle)
log("done")
}"#,
    );
    assert_eq!(out, "[harn] spawned\n[harn] done");
}

#[test]
fn test_spawn_cancel() {
    let out = run_output(
        r#"pipeline t(task) {
let handle = spawn { log("should be cancelled") }
cancel(handle)
log("cancelled")
}"#,
    );
    assert_eq!(out, "[harn] cancelled");
}

#[test]
fn test_spawn_returns_value() {
    let out = run_output("pipeline t(task) { let h = spawn { 42 }\nlet r = await(h)\nlog(r) }");
    assert_eq!(out, "[harn] 42");
}

// --- Deadline tests ---

#[test]
fn test_deadline_success() {
    let out = run_output(
        r#"pipeline t(task) {
let result = deadline 5s { log("within deadline")
42 }
log(result)
}"#,
    );
    assert_eq!(out, "[harn] within deadline\n[harn] 42");
}

#[test]
fn test_deadline_exceeded() {
    let result = run_harn_result(
        r#"pipeline t(task) {
deadline 1ms {
  var i = 0
  while i < 1000000 { i = i + 1 }
}
}"#,
    );
    assert!(result.is_err());
}

#[test]
fn test_deadline_caught_by_try() {
    let out = run_output(
        r#"pipeline t(task) {
try {
  deadline 1ms {
    var i = 0
    while i < 1000000 { i = i + 1 }
  }
} catch(e) {
  log("caught")
}
}"#,
    );
    assert_eq!(out, "[harn] caught");
}

/// Helper that runs Harn source with a set of denied builtins.
fn run_harn_with_denied(
    source: &str,
    denied: HashSet<String>,
) -> Result<(String, VmValue), VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_denied_builtins(denied);
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}

#[test]
fn test_sandbox_deny_builtin() {
    let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
    let result = run_harn_with_denied(
        r#"pipeline t(task) {
let xs = [1, 2]
push(xs, 3)
}"#,
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted, got: {msg}"
    );
    assert!(
        msg.contains("push"),
        "expected builtin name in error, got: {msg}"
    );
}

#[test]
fn test_sandbox_allowed_builtin_works() {
    // Denying "push" should not block "log"
    let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
    let result = run_harn_with_denied(r#"pipeline t(task) { log("hello") }"#, denied);
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] hello");
}

#[test]
fn test_sandbox_empty_denied_set() {
    // With an empty denied set, everything should work.
    let result = run_harn_with_denied(r#"pipeline t(task) { log("ok") }"#, HashSet::new());
    let (output, _) = result.unwrap();
    assert_eq!(output.trim(), "[harn] ok");
}

#[test]
fn test_sandbox_propagates_to_spawn() {
    // Denied builtins should propagate to spawned VMs.
    let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
    let result = run_harn_with_denied(
        r#"pipeline t(task) {
let handle = spawn {
  let xs = [1, 2]
  push(xs, 3)
}
await(handle)
}"#,
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted in spawned VM, got: {msg}"
    );
}

#[test]
fn test_sandbox_propagates_to_parallel() {
    // Denied builtins should propagate to parallel VMs.
    let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
    let result = run_harn_with_denied(
        r#"pipeline t(task) {
let results = parallel(2) { i ->
  let xs = [1, 2]
  push(xs, 3)
}
}"#,
        denied,
    );
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not permitted"),
        "expected not permitted in parallel VM, got: {msg}"
    );
}

#[test]
fn test_if_else_has_lexical_block_scope() {
    let out = run_output(
        r#"pipeline t(task) {
let x = "outer"
if true {
  let x = "inner"
  log(x)
} else {
  let x = "other"
  log(x)
}
log(x)
}"#,
    );
    assert_eq!(out, "[harn] inner\n[harn] outer");
}

#[test]
fn test_loop_and_catch_bindings_are_block_scoped() {
    let out = run_output(
        r#"pipeline t(task) {
let label = "outer"
for item in [1, 2] {
  let label = "loop ${item}"
  log(label)
}
try {
  throw("boom")
} catch (label) {
  log(label)
}
log(label)
}"#,
    );
    assert_eq!(
        out,
        "[harn] loop 1\n[harn] loop 2\n[harn] boom\n[harn] outer"
    );
}
