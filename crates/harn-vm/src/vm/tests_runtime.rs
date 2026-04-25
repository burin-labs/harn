use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use crate::compiler::Compiler;
use crate::stdlib::register_vm_stdlib;
use crate::{Chunk, InlineCacheEntry, MethodCacheTarget, PropertyCacheTarget, VmError, VmValue};
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

fn run_harn_with_chunk(source: &str) -> (Chunk, String, VmValue) {
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
                (chunk, vm.output().to_string(), result)
            })
            .await
    })
}

fn run_output(source: &str) -> String {
    run_harn(source).0.trim_end().to_string()
}

fn run_harn_at(path: &Path, source: &str) -> Result<(String, VmValue), VmError> {
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
                vm.set_source_info(&path.display().to_string(), source);
                if let Some(parent) = path.parent() {
                    vm.set_source_dir(parent);
                }
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
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

async fn run_harn_result_async(source: &str) -> Result<(String, VmValue), VmError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let chunk = Compiler::new().compile(&program).unwrap();

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    let result = vm.execute(&chunk).await?;
    Ok((vm.output().to_string(), result))
}

fn run_harn_with_setup<F>(source: &str, setup: F) -> Result<(String, VmValue), VmError>
where
    F: FnOnce(&mut Vm),
{
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
                setup(&mut vm);
                let result = vm.execute(&chunk).await?;
                Ok((vm.output().to_string(), result))
            })
            .await
    })
}

fn run_harn_with_policy(
    source: &str,
    policy: crate::orchestration::CapabilityPolicy,
) -> Result<(String, VmValue), VmError> {
    crate::orchestration::push_execution_policy(policy);
    let result = run_harn_result(source);
    crate::orchestration::pop_execution_policy();
    result
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
fn test_typed_opcode_drift_reports_type_error() {
    let err = run_vm_err(
        r#"pipeline t(task) {
  let x: int = "bad"
  log(x + 1)
}"#,
    );
    assert!(
        err.contains("Typed int add expected int operands"),
        "unexpected error: {err}"
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
        r#"pipeline t(task) { if true { log("yes") }
if false { log("wrong") } else { log("no") } }"#,
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
fn test_inline_cache_warms_property_sites() {
    let (chunk, out, _) = run_harn_with_chunk(
        r#"pipeline t(task) {
let list = [1, 2, 3]
let text = ""
let p = pair("left", "right")
var i = 0
var total = 0
while i < 3 {
  total = total + list.count
  if text.empty {
    total = total + 1
  }
  log(p.second)
  i = i + 1
}
log(total)
}"#,
    );

    assert_eq!(
        out.trim_end(),
        "[harn] right\n[harn] right\n[harn] right\n[harn] 12"
    );
    let entries = chunk.inline_cache_entries();
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::ListCount,
                ..
            }
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StringEmpty,
                ..
            }
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::PairSecond,
                ..
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_replaces_polymorphic_property_site() {
    let (chunk, out, _) = run_harn_with_chunk(
        r#"pipeline t(task) {
for value in [[1, 2], "ab"] {
  log(value.count)
}
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 2\n[harn] 2");
    let entries = chunk.inline_cache_entries();
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StringCount,
                ..
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_warms_method_sites() {
    let (chunk, out, _) = run_harn_with_chunk(
        r#"pipeline t(task) {
let list = [1, 2, 3]
let text = "abc"
let dict = {a: 1, b: 2}
let range = 1 to 3
let values = set(1, 2)
var i = 0
var total = 0
while i < 3 {
  total = total + list.count()
  total = total + text.count()
  total = total + dict.count()
  total = total + range.first()
  total = total + values.count()
  i = i + 1
}
log(total)
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 33");
    let entries = chunk.inline_cache_entries();
    for target in [
        MethodCacheTarget::ListCount,
        MethodCacheTarget::StringCount,
        MethodCacheTarget::DictCount,
        MethodCacheTarget::RangeFirst,
        MethodCacheTarget::SetCount,
    ] {
        assert!(
            entries.iter().any(|entry| matches!(
                entry,
                InlineCacheEntry::Method {
                    target: cached_target,
                    ..
                } if *cached_target == target
            )),
            "missing {target:?} in {entries:?}"
        );
    }
}

#[test]
fn test_inline_cache_warms_spread_method_site() {
    let (chunk, out, _) = run_harn_with_chunk(
        r#"pipeline t(task) {
let list = [1, 2, 3]
let args = []
var i = 0
while i < 3 {
  log(list.count(...args))
  i = i + 1
}
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 3\n[harn] 3\n[harn] 3");
    let entries = chunk.inline_cache_entries();
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Method {
                target: MethodCacheTarget::ListCount,
                ..
            }
        )),
        "{entries:?}"
    );
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
    assert!(disasm.contains("CALL_BUILTIN"));
}

#[test]
fn test_direct_builtin_call_uses_registered_sync_id() {
    let (out, _) = run_harn_with_setup(r#"pipeline t(task) { test_sync("ok") }"#, |vm| {
        vm.register_builtin("test_sync", |args, out| {
            out.push_str("sync:");
            out.push_str(&args[0].display());
            Ok(VmValue::Nil)
        });
    })
    .unwrap();
    assert_eq!(out, "sync:ok");
}

#[test]
fn test_direct_builtin_call_uses_registered_async_id() {
    let (out, _) = run_harn_with_setup(
        r#"pipeline t(task) {
let value = test_async("ok")
log(value)
}"#,
        |vm| {
            vm.register_async_builtin("test_async", |args| async move {
                Ok(VmValue::String(Rc::from(format!(
                    "async:{}",
                    args[0].display()
                ))))
            });
        },
    )
    .unwrap();
    assert_eq!(out.trim(), "[harn] async:ok");
}

#[test]
fn test_direct_builtin_callback_uses_builtin_ref_id() {
    let out = run_output(
        r#"pipeline t(task) {
let converted = ["first_name"].map(snake_to_camel)
log(converted[0])
}"#,
    );
    assert_eq!(out, "[harn] firstName");
}

#[test]
fn test_direct_builtin_call_preserves_function_shadowing() {
    let out = run_output(
        r#"pipeline t(task) {
fn push(xs, x) {
  log("shadow")
}
push([1], 2)
}"#,
    );
    assert_eq!(out, "[harn] shadow");
}

#[test]
fn test_direct_builtin_call_preserves_local_closure_shadowing() {
    let out = run_output(
        r#"pipeline t(task) {
let push = { xs, x -> log("local") }
push([1], 2)
}"#,
    );
    assert_eq!(out, "[harn] local");
}

#[test]
fn test_direct_builtin_call_falls_back_to_bridge() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let tmp = tempfile::tempdir().unwrap();
                let host_path = tmp.path().join("host.harn");
                std::fs::write(
                    &host_path,
                    r#"pub fn bridge_echo(value) { return "bridge:" + value }"#,
                )
                .unwrap();

                let mut host_vm = Vm::new();
                register_vm_stdlib(&mut host_vm);
                let bridge = crate::bridge::HostBridge::from_harn_module(host_vm, &host_path)
                    .await
                    .unwrap();

                let source = r#"pipeline t(task) { log(bridge_echo("ok")) }"#;
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().unwrap();
                let mut parser = Parser::new(tokens);
                let program = parser.parse().unwrap();
                let chunk = Compiler::new().compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_bridge(Rc::new(bridge));
                vm.execute(&chunk).await.unwrap();
                vm.output().trim().to_string()
            })
            .await
    });
    assert_eq!(out, "[harn] bridge:ok");
}

#[test]
fn test_slot_locals_preserve_shadowing_and_assignment() {
    let out = run_output(
        r#"pipeline t(task) {
var x = 1
if true {
  var x = 10
  x = x + 1
  log(x)
}
x = x + 2
log(x)
}"#,
    );
    assert_eq!(out, "[harn] 11\n[harn] 3");
}

#[test]
fn test_slot_params_and_recursive_function_calls() {
    let out = run_output(
        r#"pipeline t(task) {
fn sum_to(n, acc = 0) {
  if n <= 0 {
    return acc
  }
  return sum_to(n - 1, acc + n)
}
log(sum_to(5))
}"#,
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_slot_locals_sync_for_closure_capture() {
    let out = run_output(
        r#"pipeline t(task) {
var x = 1
x = 7
let f = { -> x + 1 }
log(f())
}"#,
    );
    assert_eq!(out, "[harn] 8");
}

#[test]
fn test_slot_property_assignment_updates_slot_value() {
    let out = run_output(
        r#"pipeline t(task) {
var d = {count: 1}
d.count = d.count + 2
log(d.count)
}"#,
    );
    assert_eq!(out, "[harn] 3");
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
fn test_cancel_graceful_propagates_to_cpu_bound_spawn() {
    let out = run_output(
        r#"pipeline t(task) {
let handle = spawn {
  var i = 0
  while true {
    i = i + 1
  }
}
let result = cancel_graceful(handle, 100ms)
log(is_err(result))
log(contains(unwrap_err(result), "cancelled"))
}"#,
    );
    assert_eq!(out, "[harn] true\n[harn] true");
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_deadline_interrupts_async_sleep_without_wall_clock() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r#"pipeline t(task) {
try {
  deadline 50ms {
    sleep(1s)
    log("missed deadline")
  }
} catch(e) {
  log("caught")
}
}"#,
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(50)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] caught");
        })
        .await;
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
fn test_policy_workspace_roots_catch_filesystem_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    let outside_copy = outside.path().join("copy.txt");
    let outside_new = outside.path().join("new.txt");
    let outside_dir = outside.path().join("new_dir");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec![
                "read_text".to_string(),
                "list".to_string(),
                "exists".to_string(),
                "write_text".to_string(),
                "delete".to_string(),
            ],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    let escapes = [
        format!(
            r#"pipeline t(task) {{ read_file("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ read_file_bytes("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
            outside_new.display()
        ),
        format!(
            r#"pipeline t(task) {{ append_file("{}", "x") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ copy_file("{}", "{}") }}"#,
            outside_file.display(),
            allowed.path().join("copy.txt").display()
        ),
        format!(
            r#"pipeline t(task) {{ copy_file("{}", "{}") }}"#,
            allowed.path().join("missing.txt").display(),
            outside_copy.display()
        ),
        format!(
            r#"pipeline t(task) {{ list_dir("{}") }}"#,
            outside.path().display()
        ),
        format!(
            r#"pipeline t(task) {{ mkdir("{}") }}"#,
            outside_dir.display()
        ),
        format!(
            r#"pipeline t(task) {{ stat("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ delete_file("{}") }}"#,
            outside_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ file_exists("{}") }}"#,
            outside_file.display()
        ),
    ];

    for source in escapes {
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            matches!(
                err,
                VmError::CategorizedError {
                    category: crate::value::ErrorCategory::ToolRejected,
                    ..
                }
            ),
            "expected tool_rejected for source {source}, got {err:?}"
        );
        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation message, got {err}"
        );
    }
}

#[test]
fn test_policy_workspace_roots_reject_process_cwd_escape() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };

    let source = format!(
        r#"pipeline t(task) {{ exec_at("{}", "sh", "-c", "true") }}"#,
        outside.path().display()
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();
    assert!(matches!(
        err,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
    assert!(err.to_string().contains("process cwd"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_macos_process_sandbox_surfaces_denial_as_typed_error() {
    if !std::path::Path::new("/usr/bin/sandbox-exec").exists() {
        return;
    }
    let cwd = std::env::current_dir().unwrap();
    let allowed = tempfile::tempdir_in(&cwd).unwrap();
    let outside_base = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| cwd.parent().unwrap_or(cwd.as_path()).to_path_buf());
    if outside_base.starts_with("/tmp") || outside_base.starts_with("/private/tmp") {
        return;
    }
    let outside = tempfile::tempdir_in(outside_base).unwrap();
    let outside_file = outside.path().join("blocked.txt");
    let previous = std::env::var("HARN_HANDLER_SANDBOX").ok();
    std::env::set_var("HARN_HANDLER_SANDBOX", "enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let source = format!(
        r#"pipeline t(task) {{ shell("printf denied > '{}'") }}"#,
        outside_file.display()
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();
    match previous {
        Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
        None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
    }

    assert!(matches!(
        err,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
    assert!(err.to_string().contains("sandbox violation"));
    assert!(!outside_file.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn test_linux_process_sandbox_catches_ten_process_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    let outside_new = outside.path().join("new.txt");
    let outside_copy = outside.path().join("copy.txt");
    let outside_dir = outside.path().join("new_dir");
    let allowed_file = allowed.path().join("allowed.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    std::fs::write(&allowed_file, "allowed").unwrap();

    let previous = std::env::var("HARN_HANDLER_SANDBOX").ok();
    std::env::set_var("HARN_HANDLER_SANDBOX", "enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["exec".to_string()]),
            (
                "workspace".to_string(),
                vec![
                    "read_text".to_string(),
                    "list".to_string(),
                    "exists".to_string(),
                    "write_text".to_string(),
                    "delete".to_string(),
                ],
            ),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };

    let escapes = [
        format!("cat {}", shell_quote(&outside_file)),
        format!("printf x > {}", shell_quote(&outside_new)),
        format!("printf x >> {}", shell_quote(&outside_file)),
        format!("mkdir {}", shell_quote(&outside_dir)),
        format!("rm {}", shell_quote(&outside_file)),
        format!(
            "cp {} {}",
            shell_quote(&outside_file),
            shell_quote(&allowed.path().join("copy.txt"))
        ),
        format!(
            "cp {} {}",
            shell_quote(&allowed_file),
            shell_quote(&outside_copy)
        ),
        format!(
            "mv {} {}",
            shell_quote(&allowed_file),
            shell_quote(&outside.path().join("moved.txt"))
        ),
        format!(
            "ln -s {} {} && cat {}",
            shell_quote(&outside_file),
            shell_quote(&allowed.path().join("link.txt")),
            shell_quote(&allowed.path().join("link.txt"))
        ),
        format!("touch {}", shell_quote(&outside.path().join("touched.txt"))),
    ];
    assert_eq!(escapes.len(), 10);

    for command in escapes {
        let source = format!(
            r#"pipeline t(task) {{ shell("{}") }}"#,
            harn_string_escape(&command)
        );
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            matches!(
                err,
                VmError::CategorizedError {
                    category: crate::value::ErrorCategory::ToolRejected,
                    ..
                }
            ),
            "expected tool_rejected for command {command}, got {err:?}"
        );
        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation for command {command}, got {err}"
        );
    }

    match previous {
        Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
        None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
    }
    assert!(outside_file.exists());
    assert!(!outside_new.exists());
    assert!(!outside_copy.exists());
    assert!(!outside_dir.exists());
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_allows_process_exec_in_workspace() {
    let allowed = tempfile::tempdir().unwrap();
    let allowed_file = allowed.path().join("allowed.txt");
    let previous = std::env::var("HARN_HANDLER_SANDBOX").ok();
    std::env::set_var("HARN_HANDLER_SANDBOX", "enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["exec".to_string()]),
            ("workspace".to_string(), vec!["write_text".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let command = format!("echo allowed> {}", windows_cmd_quote(&allowed_file));
    let source = format!(
        r#"pipeline t(task) {{ shell("{}") }}"#,
        harn_string_escape(&command)
    );
    let result = run_harn_with_policy(&source, policy);

    match previous {
        Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
        None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
    }

    result.unwrap();
    assert!(allowed_file.exists());
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_allows_exec_argv0() {
    let allowed = tempfile::tempdir().unwrap();
    let previous = std::env::var("HARN_HANDLER_SANDBOX").ok();
    std::env::set_var("HARN_HANDLER_SANDBOX", "enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "process".to_string(),
            vec!["exec".to_string()],
        )]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let result = run_harn_with_policy(
        r#"pipeline t(task) { exec("cmd", "/C", "exit 0") }"#,
        policy,
    );

    match previous {
        Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
        None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
    }

    result.unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_sandbox_denies_write_outside_workspace() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("blocked.txt");
    let previous = std::env::var("HARN_HANDLER_SANDBOX").ok();
    std::env::set_var("HARN_HANDLER_SANDBOX", "enforce");

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["exec".to_string()]),
            ("workspace".to_string(), vec!["write_text".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..Default::default()
    };
    let command = format!("echo denied> {}", windows_cmd_quote(&outside_file));
    let source = format!(
        r#"pipeline t(task) {{ shell("{}") }}"#,
        harn_string_escape(&command)
    );
    let err = run_harn_with_policy(&source, policy).unwrap_err();

    match previous {
        Some(value) => std::env::set_var("HARN_HANDLER_SANDBOX", value),
        None => std::env::remove_var("HARN_HANDLER_SANDBOX"),
    }

    assert!(matches!(
        err,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
    assert!(
        err.to_string().contains("sandbox violation")
            || err.to_string().contains("process sandbox failed"),
        "expected sandbox denial, got {err}"
    );
    assert!(!outside_file.exists());
}

#[cfg(target_os = "windows")]
fn windows_cmd_quote(path: &std::path::Path) -> String {
    format!(r#""{}""#, path.display())
}

#[cfg(target_os = "linux")]
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn harn_string_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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

#[test]
fn package_export_import_executes_through_manifest_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".harn/packages/acme/runtime")).unwrap();
    std::fs::write(
        root.join(".harn/packages/acme/harn.toml"),
        "[exports]\ncapabilities = \"runtime/capabilities.harn\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".harn/packages/acme/runtime/capabilities.harn"),
        "pub fn exported_capability() { return 41 + 1 }\n",
    )
    .unwrap();
    let entry = root.join("main.harn");
    let source = r#"
import "acme/capabilities"

pipeline main(task) {
  println(exported_capability())
}
"#;

    let (out, _) = run_harn_at(&entry, source).unwrap();
    assert_eq!(out.trim(), "42");
}
