use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::compiler::{Compiler, CompilerOptions};
use crate::stdlib::register_vm_stdlib;
use crate::{
    AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, DirectCallState, InlineCacheEntry,
    MethodCacheTarget, PropertyCacheTarget, VmError, VmValue,
};
use harn_lexer::Lexer;
use harn_parser::Parser;

use super::*;

fn run_harn(source: &str) -> (String, VmValue) {
    run_harn_with_options(source, CompilerOptions::optimized())
}

fn run_harn_with_options(source: &str, options: CompilerOptions) -> (String, VmValue) {
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
                let chunk = Compiler::with_options(options).compile(&program).unwrap();

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm.execute(&chunk).await.unwrap();
                (vm.output().to_string(), result)
            })
            .await
    })
}

fn run_harn_result_display_with_options(
    source: &str,
    options: CompilerOptions,
) -> Result<(String, String), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut lexer = Lexer::new(source);
                let tokens = lexer.tokenize().map_err(|error| error.to_string())?;
                let mut parser = Parser::new(tokens);
                let program = parser.parse().map_err(|error| error.to_string())?;
                let chunk = Compiler::with_options(options)
                    .compile(&program)
                    .map_err(|error| error.to_string())?;

                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let result = vm
                    .execute(&chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok((vm.output().to_string(), result.display()))
            })
            .await
    })
}

fn run_harn_with_inline_cache_entries(source: &str) -> (Vec<InlineCacheEntry>, String, VmValue) {
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
                vm.set_harness(crate::Harness::real());
                let result = vm.execute(&chunk).await.unwrap();
                let mut inline_cache_entries = vm.inline_cache_entries_for_chunk(&chunk);
                for (cache_id, entries) in &vm.inline_caches {
                    if *cache_id != chunk.cache_id() {
                        inline_cache_entries.extend(entries.clone());
                    }
                }
                (inline_cache_entries, vm.output().to_string(), result)
            })
            .await
    })
}

fn run_output(source: &str) -> String {
    run_harn(source).0.trim_end().to_string()
}

#[test]
fn optimizer_differential_success_programs_match() {
    let programs = [
        r#"pipeline test(task) {
  __io_println(2 + 3 * 4)
  __io_println("ha" * 2)
  __io_println(([1] + [2, 3])[2])
  __io_println(({a: 1} + {b: 2}).b)
  __io_println((true && false) || !false)
}"#,
        r"pipeline test(task) {
  fn add(a: int, b: int = 4) {
    return a + b
  }
  let base = 3
  __io_println(add(base))
  __io_println(add(1 + 1, 2 + 2))
}",
    ];

    for source in programs {
        let optimized =
            run_harn_result_display_with_options(source, CompilerOptions::optimized()).unwrap();
        let unoptimized =
            run_harn_result_display_with_options(source, CompilerOptions::without_optimizations())
                .unwrap();
        assert_eq!(optimized, unoptimized, "{source}");
    }
}

#[test]
fn optimizer_differential_errors_match() {
    let source = "pipeline test(task) { __io_println(1 / 0) }";
    let optimized =
        run_harn_result_display_with_options(source, CompilerOptions::optimized()).unwrap_err();
    let unoptimized =
        run_harn_result_display_with_options(source, CompilerOptions::without_optimizations())
            .unwrap_err();

    assert_eq!(optimized, unoptimized);
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

#[test]
fn runtime_error_renderer_normalizes_frame_paths() {
    let mut vm = Vm::new();
    vm.set_source_info(
        "/workspace/pipelines/mode/../mode/auto.harn",
        "let run = agent_loop(message, system_prompt, opts)\n",
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
                    Err(e) => format!("{e}"),
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
  let x: int = "bad"
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
fn test_string_repeat_operator_rejects_oversized_count() {
    // `"a" * <huge>` must error cleanly, never OOM / panic `capacity overflow`.
    let result = run_harn_result(r#"pipeline t(task) { let s = "ab" * 9999999999 }"#);
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_string_repeat_method_rejects_oversized_count() {
    let result = run_harn_result(r#"pipeline t(task) { let s = "ab".repeat(9999999999) }"#);
    let err = result.unwrap_err().to_string();
    assert!(err.contains("repeat") && err.contains("limit"), "{err}");
}

#[test]
fn test_str_pad_rejects_oversized_width() {
    let result = run_harn_result(r#"pipeline t(task) { let s = str_pad("x", 9999999999, "-") }"#);
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
    let out = run_output(
        r#"pipeline t(task) {
let arr = [10, 20, 30]
let dict = {name: "harn"}
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
fn test_list_properties() {
    let out = run_output(
        "pipeline t(task) { let list = [1, 2, 3]\nlog(list.count)\nlog(list.empty)\nlog(list.first)\nlog(list.last) }",
    );
    assert_eq!(out, "[harn] 3\n[harn] false\n[harn] 1\n[harn] 3");
}

#[test]
fn test_inline_cache_warms_property_sites() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
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
fn test_inline_cache_warms_dict_and_struct_property_sites() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
struct Point {
  x: int
  y: int
}
let record = {hot: 7}
let point = Point {x: 2, y: 3}
var i = 0
var total = 0
while i < 3 {
  total = total + record.hot + point.y
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 30");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::DictField(name),
                ..
            } if name.as_ref() == "hot"
        )),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::Property {
                target: PropertyCacheTarget::StructField { field_name, index },
                ..
            } if field_name.as_ref() == "y" && *index == 1
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_replaces_polymorphic_property_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r#"pipeline t(task) {
for value in [[1, 2], "ab"] {
  log(value.count)
}
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 2\n[harn] 2");
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
    let (entries, out, _) = run_harn_with_inline_cache_entries(
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
  if list.contains(i + 1) { total = total + 1 }
  if text.contains("b") { total = total + 1 }
  if dict.has("a") { total = total + 1 }
  if values.contains(2) { total = total + 1 }
  i = i + 1
}
log(total)
}"#,
    );

    assert_eq!(out.trim_end(), "[harn] 45");
    for target in [
        MethodCacheTarget::ListCount,
        MethodCacheTarget::ListContains,
        MethodCacheTarget::StringCount,
        MethodCacheTarget::StringContains,
        MethodCacheTarget::DictCount,
        MethodCacheTarget::DictHas,
        MethodCacheTarget::RangeFirst,
        MethodCacheTarget::SetCount,
        MethodCacheTarget::SetContains,
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
fn test_inline_cache_warms_harness_property_and_method_sites() {
    let (entries, out, result) = run_harn_with_inline_cache_entries(
        r#"fn main(harness: Harness) {
  var i = 0
  var hits = 0
  while i < 3 {
    if harness.env.get_or("__HARN_TEST_MISSING__", "fallback") == "fallback" {
      hits = hits + 1
    }
    let _ = harness.clock.monotonic_ms()
    i = i + 1
  }
  return hits
}"#,
    );

    assert_eq!(out.trim_end(), "");
    assert!(matches!(result, VmValue::Int(3)));
    for target in [
        PropertyCacheTarget::HarnessSubHandle(crate::HarnessKind::Env),
        PropertyCacheTarget::HarnessSubHandle(crate::HarnessKind::Clock),
    ] {
        assert!(
            entries.iter().any(|entry| matches!(
                entry,
                InlineCacheEntry::Property {
                    target: cached_target,
                    ..
                } if *cached_target == target
            )),
            "missing {target:?} in {entries:?}"
        );
    }
    for target in [
        MethodCacheTarget::Harness(crate::HarnessKind::Env),
        MethodCacheTarget::Harness(crate::HarnessKind::Clock),
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
fn test_adaptive_inline_cache_specializes_generic_integer_add_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
var i = erase(0)
var total = erase(0)
while i < erase(8) {
  total = total + i
  i = i + erase(1)
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 28");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Int,
                    hits,
                    ..
                },
            } if *hits >= 3
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_deoptimizes_mixed_binary_shapes() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn erase(x) {
  return x
}
let values = [erase(1), erase(2), erase(3), erase(4.0), erase(5.0)]
var acc = erase(0)
for value in values {
  acc = acc + value
}
log(acc)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 15.0");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Float,
                    misses: 1,
                    ..
                },
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_specializes_named_closure_call_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn inc(x) {
  return x + 1
}
var i = 0
var total = 0
while i < 8 {
  total = total + inc(i)
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 36");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized { hits, .. },
            } if *hits >= 3
        )),
        "{entries:?}"
    );
}

#[test]
fn test_adaptive_inline_cache_deoptimizes_rebound_closure_call_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
fn inc(x) {
  return x + 1
}
fn dec(x) {
  return x - 1
}
var op = inc
var i = 0
var total = 0
while i < 5 {
  if i == 3 {
    op = dec
  }
  total = total + op(10)
  i = i + 1
}
log(total)
}",
    );

    assert_eq!(out.trim_end(), "[harn] 51");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized { misses: 1, .. },
            }
        )),
        "{entries:?}"
    );
}

#[test]
fn test_inline_cache_warms_spread_method_site() {
    let (entries, out, _) = run_harn_with_inline_cache_entries(
        r"pipeline t(task) {
let list = [1, 2, 3]
let args = []
var i = 0
while i < 3 {
  log(list.count(...args))
  i = i + 1
}
}",
    );

    assert_eq!(out.trim_end(), "[harn] 3\n[harn] 3\n[harn] 3");
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
    let chunk = Compiler::with_options(CompilerOptions::without_optimizations())
        .compile(&program)
        .unwrap();
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
            vm.register_async_builtin("test_async", |_ctx, args| async move {
                Ok(VmValue::String(std::sync::Arc::from(format!(
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
                vm.set_bridge(Arc::new(bridge));
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
        r"pipeline t(task) {
var x = 1
if true {
  var x = 10
  x = x + 1
  log(x)
}
x = x + 2
log(x)
}",
    );
    assert_eq!(out, "[harn] 11\n[harn] 3");
}

#[test]
fn test_slot_params_and_recursive_function_calls() {
    let out = run_output(
        r"pipeline t(task) {
fn sum_to(n, acc = 0) {
  if n <= 0 {
    return acc
  }
  return sum_to(n - 1, acc + n)
}
log(sum_to(5))
}",
    );
    assert_eq!(out, "[harn] 15");
}

#[test]
fn test_slot_locals_sync_for_closure_capture() {
    let out = run_output(
        r"pipeline t(task) {
var x = 1
x = 7
let f = { -> x + 1 }
log(f())
}",
    );
    assert_eq!(out, "[harn] 8");
}

#[test]
fn test_slot_property_assignment_updates_slot_value() {
    let out = run_output(
        r"pipeline t(task) {
var d = {count: 1}
d.count = d.count + 2
log(d.count)
}",
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
        r"pipeline t(task) {
var result = 0
try { result = 42 } catch(e) { result = 0 }
log(result)
}",
    );
    assert_eq!(out, "[harn] 42");
}

#[test]
fn test_throw_uncaught() {
    let result = run_harn_result(r#"pipeline t(task) { throw "boom" }"#);
    assert!(result.is_err());
}

#[test]
fn test_runtime_user_call_arg_type_mismatch() {
    let result = run_harn_result(
        r#"pipeline t(task) {
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
        r#"pipeline t(task) {
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
        r#"pipeline t(task) {
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
        r#"pipeline t(task) {
fn first<T>(xs: list<T>) -> T { return xs[0] }
return first(["ok"])
}"#,
    )
    .unwrap();
    assert!(matches!(result, VmValue::String(s) if s.as_ref() == "ok"));
}

#[test]
fn test_runtime_user_call_missing_required_arg_rejected() {
    let result = run_harn_result(
        r"pipeline t(task) {
fn echo(value) { return value }
echo()
}",
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Arity mismatch: 'echo'"), "{err}");
    assert!(err.contains("expects 1 argument, got 0"), "{err}");
}

#[test]
fn test_runtime_builtin_call_arg_type_mismatch() {
    let result = run_harn_result(r"pipeline t(task) { lowercase(42) }");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("'lowercase' parameter `text` expects string"),
        "{err}"
    );
    assert!(err.contains("got int"), "{err}");
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
    assert!(out.contains('1'));
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
        "Expected stack overflow error, got: {err}"
    );
}

#[test]
fn test_division_by_zero() {
    let err = run_vm_err("pipeline default(task) { log(1 / 0) }");
    assert!(
        err.contains("Division by zero") || err.contains("division"),
        "Expected division by zero error, got: {err}"
    );
}

#[test]
fn test_int_division_overflow_wraps_instead_of_panicking() {
    let out = run_output(
        r"pipeline default(task) {
  let min = -9223372036854775807 - 1
  log(min / -1)
  log(min % -1)
}",
    );
    assert_eq!(out, "[harn] -9223372036854775808\n[harn] 0");
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_each_stream_break_cancels_remaining_work() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let handle = tokio::task::spawn_local(async {
                run_harn_result_async(
                    r"pipeline t(task) {
let completed = atomic(0)
let results = parallel each [1, 2, 3] with { max_concurrent: 1 } { item ->
  sleep(1s)
  atomic_add(completed, 1)
  return item
} as stream
for item in results {
  break
}
sleep(3s)
log(atomic_get(completed))
}",
                )
                .await
            });
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(4)).await;
            let (output, _) = handle.await.expect("join VM task").expect("run Harn");
            assert_eq!(output.trim_end(), "[harn] 1");
        })
        .await;
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
fn test_std_signal_handlers_are_lifo_and_removable() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t() {
  let first = on_interrupt({ -> log("a") }, {once: false})
  let second = on_interrupt({ -> log("b") }, {once: false})
  __signal_raise("SIGINT")
  off_interrupt(second)
  __signal_raise("SIGINT")
  log(interrupted())
  off_interrupt(first.handle)
}
"#,
    );
    assert_eq!(out, "[harn] b\n[harn] a\n[harn] a\n[harn] true");
}

#[test]
fn test_with_interrupt_unregisters_after_throw() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t() {
  try {
    with_interrupt({ -> log("leaked") }, { -> throw "boom" }, {once: false})
  } catch (e) {
  }
  let raised = try {
    __signal_raise("SIGINT")
    "not interrupted"
  } catch (e) {
    "interrupted"
  }
  log(raised)
}
"#,
    );
    assert_eq!(out, "[harn] interrupted");
}

#[test]
fn test_interrupt_handler_graceful_timeout_is_enforced() {
    let out = run_output(
        r#"
import "std/signal"

pipeline t() {
  on_interrupt({ ->
    var spin = 0
    while true { spin = spin + 1 }
  }, {graceful_timeout_ms: 0})
  let result = try {
    __signal_raise("SIGINT")
    "missed timeout"
  } catch (e) {
    e
  }
  log(result)
}
"#,
    );
    assert_eq!(out, "[harn] kind:interrupted:handler_timeout");
}

#[test]
fn test_host_signal_token_dispatches_matching_signal() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut vm = Vm::new();
        vm.register_builtin("term_marker", |_, out| {
            out.push_str("[harn] term\n");
            Ok(VmValue::Nil)
        });
        vm.register_builtin("int_marker", |_, out| {
            out.push_str("[harn] int\n");
            Ok(VmValue::Nil)
        });
        let term_options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                std::sync::Arc::from("SIGTERM"),
            )])),
        )])));
        let int_options = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
            "signals".to_string(),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                std::sync::Arc::from("SIGINT"),
            )])),
        )])));
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(std::sync::Arc::from("term_marker")),
            Some(&term_options),
        )
        .unwrap();
        vm.register_interrupt_handler(
            VmValue::BuiltinRef(std::sync::Arc::from("int_marker")),
            Some(&int_options),
        )
        .unwrap();

        let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let signal_token = std::sync::Arc::new(std::sync::Mutex::new(Some("SIGTERM".to_string())));
        vm.install_interrupt_signal_token(signal_token);
        vm.install_cancel_token(cancel_token);

        assert!(vm.pending_scope_interrupt().await.is_none());
        assert_eq!(vm.output().trim_end(), "[harn] term");
    });
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
        r"pipeline t(task) {
deadline 1ms {
  var i = 0
  while i < 1000000 { i = i + 1 }
}
}",
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_cancel_during_await_aborts_spawned_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let source = r"pipeline t(task) {
let handle = spawn {
  sleep(1s)
  mark()
}
await(handle)
}";
            let mut lexer = Lexer::new(source);
            let tokens = lexer.tokenize().unwrap();
            let mut parser = Parser::new(tokens);
            let program = parser.parse().unwrap();
            let chunk = Compiler::new().compile(&program).unwrap();

            let marker = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let marker_for_builtin = marker.clone();
            let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let vm_cancel_token = cancel_token.clone();
            let handle = tokio::task::spawn_local(async move {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.register_builtin("mark", move |_, _| {
                    marker_for_builtin.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(VmValue::Nil)
                });
                vm.install_cancel_token(vm_cancel_token);
                let result = vm.execute(&chunk).await;
                (vm.output().to_string(), result)
            });

            tokio::task::yield_now().await;
            cancel_token.store(true, std::sync::atomic::Ordering::SeqCst);
            tokio::time::advance(Duration::from_millis(300)).await;
            let (output, result) = handle.await.expect("join VM task");
            assert!(output.is_empty());
            let error = result.expect_err("parent await should be cancelled");
            assert!(error.to_string().contains("kind:cancelled"));

            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert!(
                !marker.load(std::sync::atomic::Ordering::SeqCst),
                "spawned task should be aborted when parent await is cancelled"
            );
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
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(task) {
let xs = [1, 2]
push(xs, 3)
}",
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
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
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
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(task) {
let handle = spawn {
  let xs = [1, 2]
  push(xs, 3)
}
await(handle)
}",
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
    let denied: HashSet<String> = std::iter::once("push".to_string()).collect();
    let result = run_harn_with_denied(
        r"pipeline t(task) {
let results = parallel(2) { i ->
  let xs = [1, 2]
  push(xs, 3)
}
}",
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

    // `file_exists`/`exists` is the one read-scope probe that soft-falses
    // instead of throwing on an out-of-root path (v0.8.55): an absent path and
    // an out-of-sandbox path are indistinguishable to a caller, so the safe,
    // non-leaky answer is `false` rather than a sandbox violation. Content
    // reads (`read_file`, asserted above) still error — only presence probing
    // is softened.
    let (_, exists_outside) = run_harn_with_policy(
        &format!(
            r#"pipeline t(task) {{ file_exists("{}") }}"#,
            outside_file.display()
        ),
        policy,
    )
    .expect("file_exists outside sandbox should soft-false, not error");
    assert!(
        matches!(exists_outside, VmValue::Bool(false)),
        "file_exists on an out-of-root path must read as absent, got {exists_outside:?}"
    );
}

#[test]
fn test_policy_read_only_root_allows_reads_but_rejects_writes() {
    let writable = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    let read_only_file = read_only.path().join("memory.txt");
    std::fs::write(&read_only_file, "secret").unwrap();

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
        workspace_roots: vec![writable.path().display().to_string()],
        read_only_roots: vec![read_only.path().display().to_string()],
        side_effect_level: Some("workspace_write".to_string()),
        ..Default::default()
    };

    // Reading from a read-only root succeeds.
    let read_source = format!(
        r#"pipeline t(task) {{ return read_file("{}") }}"#,
        read_only_file.display()
    );
    let (_out, value) = run_harn_with_policy(&read_source, policy.clone()).unwrap();
    assert_eq!(value.display(), "secret");

    // Mutating an existing file under a read-only root is rejected with
    // the read-only-specific message even though the workspace grants
    // write_text/delete. These target the existing file so the path
    // canonicalizes identically on every platform (Windows resolves a
    // non-existent target to a `\\?\` verbatim path that the generic
    // out-of-scope branch reports instead — still rejected, just a
    // coarser message; the path-scope logic itself is covered for the
    // non-existent case by the `sandbox_hardened` integration test).
    let existing_mutations = [
        format!(
            r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ append_file("{}", "x") }}"#,
            read_only_file.display()
        ),
        format!(
            r#"pipeline t(task) {{ delete_file("{}") }}"#,
            read_only_file.display()
        ),
    ];
    for source in existing_mutations {
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
            err.to_string().contains("read-only workspace root"),
            "expected read-only rejection message, got {err}"
        );
    }

    // Creating a new file under a read-only root is likewise rejected.
    let create = format!(
        r#"pipeline t(task) {{ write_file("{}", "x") }}"#,
        read_only.path().join("new.txt").display()
    );
    let err = run_harn_with_policy(&create, policy).unwrap_err();
    assert!(
        matches!(
            err,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ),
        "creating a new file under a read-only root must be rejected, got {err:?}"
    );
    assert!(
        err.to_string().contains("sandbox violation"),
        "expected sandbox violation, got {err}"
    );
    assert!(
        !read_only.path().join("new.txt").exists(),
        "rejected write must not touch disk"
    );
}

#[test]
fn test_policy_workspace_roots_catch_template_render_escapes() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_template = outside.path().join("secret.harn.prompt");
    std::fs::write(&outside_template, "TOP_SECRET_RENDER_BYPASS").unwrap();

    let policy = crate::orchestration::CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("workspace".to_string(), vec!["read_text".to_string()]),
            ("template".to_string(), vec!["render".to_string()]),
        ]),
        workspace_roots: vec![allowed.path().display().to_string()],
        side_effect_level: Some("read_only".to_string()),
        ..Default::default()
    };

    let escaped_path = outside_template.display();
    let escapes = [
        format!(r#"pipeline t(task) {{ render("{escaped_path}") }}"#),
        format!(r#"pipeline t(task) {{ render_prompt("{escaped_path}") }}"#),
        format!(r#"pipeline t(task) {{ render_with_provenance("{escaped_path}") }}"#),
        format!(
            r#"pipeline t(task) {{ host_call("template.render", {{path: "{escaped_path}"}}) }}"#
        ),
    ];

    for source in escapes {
        let err = run_harn_with_policy(&source, policy.clone()).unwrap_err();
        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation for source {source}, got {err}"
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
    let command = format!("echo allowed>{}", allowed_file.display());
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
    let command = format!("echo denied>{}", outside_file.display());
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
  __io_println(exported_capability())
}
"#;

    let (out, _) = run_harn_at(&entry, source).unwrap();
    assert_eq!(out.trim(), "42");
}

// --- Closure late-bind walk skip (Chunk::references_outer_names) ---
//
// The runtime fast path in `Vm::closure_call_env` skips the
// caller-scope late-bind walk for callees whose bodies never read
// outer names. These tests pin the corner cases that would regress
// if the static check ever stopped flagging an env-reading opcode:
// self-recursion, mutual recursion, sibling-fn references, var
// rebinding from inside a closure, and inline lambdas as callback
// arguments.

#[test]
fn inline_arithmetic_lambda_map_filter_optimization_path() {
    let out = run_vm(
        r"pipeline default(task) {
            let evens = [1, 2, 3, 4, 5, 6].filter({ x -> x % 2 == 0 })
            let doubled = evens.map({ x -> x * 2 })
            log(doubled)
        }",
    );
    assert_eq!(out, "[harn] [4, 8, 12]\n");
}

#[test]
fn self_recursive_named_fn_still_resolves_after_optimization() {
    let out = run_vm(
        r"pipeline default(task) {
            fn fact(n) {
                if n <= 1 { return 1 }
                return n * fact(n - 1)
            }
            log(fact(6))
        }",
    );
    assert_eq!(out, "[harn] 720\n");
}

#[test]
fn mutually_recursive_named_fns_still_resolve_after_optimization() {
    // `return is_odd(n - 1)` compiles to `Op::Constant + Op::TailCall`
    // (see compile_return in compiler/statements.rs) — TailCall is
    // in the flag set so the late-bind walk runs and the cross-fn
    // resolution succeeds.
    let out = run_vm(
        r"pipeline default(task) {
            fn is_even(n) {
                if n == 0 { return true }
                return is_odd(n - 1)
            }
            fn is_odd(n) {
                if n == 0 { return false }
                return is_even(n - 1)
            }
            log(is_even(4))
            log(is_even(5))
        }",
    );
    assert_eq!(out, "[harn] true\n[harn] false\n");
}

#[test]
fn anonymous_lambda_calling_sibling_fn_via_call_builtin_flags() {
    let out = run_vm(
        r"pipeline default(task) {
            fn helper(x) { return x + 100 }
            let r = [1, 2, 3].map({ v -> helper(v) })
            log(r)
        }",
    );
    assert_eq!(out, "[harn] [101, 102, 103]\n");
}

#[test]
fn anonymous_lambda_with_get_var_capture_flags() {
    let out = run_vm(
        r"pipeline default(task) {
            let bonus = 10
            let r = [1, 2, 3].map({ v -> v + bonus })
            log(r)
        }",
    );
    assert_eq!(out, "[harn] [11, 12, 13]\n");
}

#[test]
fn pure_lambda_inside_pipeline_with_unrelated_locals_skips_walk() {
    let out = run_vm(
        r"pipeline default(task) {
            fn helper_a(x) { return x + 1 }
            fn helper_b(x) { return x + 2 }
            let r = [10, 20, 30].map({ v -> v * 2 })
            log(r)
            log(helper_a(0))
            log(helper_b(0))
        }",
    );
    assert_eq!(out, "[harn] [20, 40, 60]\n[harn] 1\n[harn] 2\n");
}

#[test]
fn nested_map_lambdas_skip_walk_independently() {
    let out = run_vm(
        r"pipeline default(task) {
            let grid = [[1, 2], [3, 4]]
            let r = grid.map({ row -> row.map({ x -> x * 10 }) })
            log(r)
        }",
    );
    assert_eq!(out, "[harn] [[10, 20], [30, 40]]\n");
}

#[test]
fn typed_param_lambda_uses_check_type_and_walks() {
    let out = run_vm(
        r"pipeline default(task) {
            let r = [1, 2, 3].map({ v: int -> v + 1 })
            log(r)
        }",
    );
    assert_eq!(out, "[harn] [2, 3, 4]\n");
}

/// Regression: a `var` inferred `int` from its initializer but later reassigned
/// through an `any`-typed value of a different primitive must not be specialized
/// into a typed opcode (`AddInt`), which would hard-error at runtime on a
/// program the generic path runs correctly. The optimized result must match the
/// unoptimized one exactly. (Previously the optimizer threw
/// "Typed int add expected int operands, got int and float".)
#[test]
fn var_reassigned_via_any_matches_unoptimized() {
    let source = r#"pipeline default(task) {
  var x = 0
  var sum = 0
  var i = 0
  let cell = shared_cell("k", 2.5)
  while i < 3 {
    sum = sum + x
    if i == 0 { x = shared_get(cell) }
    i = i + 1
  }
  log("${sum}")
}"#;
    let optimized = run_harn_result_display_with_options(source, CompilerOptions::optimized())
        .expect("optimized run should not spuriously type-error");
    let baseline =
        run_harn_result_display_with_options(source, CompilerOptions::without_optimizations())
            .expect("unoptimized run is the ground truth");
    assert_eq!(
        optimized.0, baseline.0,
        "stdout must match the generic path"
    );
    assert_eq!(optimized.0.trim_end(), "[harn] 5.0");
}

/// Companion to the above for the `for`-item binding, which is reassignable per
/// iteration. Reassigning it from an `any` value previously crashed under the
/// optimizer; it must now match the unoptimized result.
#[test]
fn for_item_reassigned_via_any_matches_unoptimized() {
    let source = r#"pipeline default(task) {
  var sum = 0
  let cell = shared_cell("k", 2.5)
  for n in [1, 2, 3] {
    sum = sum + n
    n = shared_get(cell)
    sum = sum + n
  }
  log("${sum}")
}"#;
    let optimized = run_harn_result_display_with_options(source, CompilerOptions::optimized())
        .expect("optimized run should not spuriously type-error");
    let baseline =
        run_harn_result_display_with_options(source, CompilerOptions::without_optimizations())
            .expect("unoptimized run is the ground truth");
    assert_eq!(
        optimized.0, baseline.0,
        "stdout must match the generic path"
    );
}

/// The monomorphic loop counter / accumulator idiom keeps producing the right
/// result with the typed fast path engaged (guards against the gate
/// over-demoting and silently changing arithmetic results).
#[test]
fn monomorphic_counter_loop_result_is_correct() {
    let source = r#"pipeline default(task) {
  var i = 0
  var total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
  log("${total}")
}"#;
    let (out, _) = run_harn(source);
    assert_eq!(out.trim_end(), "[harn] 140");
}
