use super::*;
use crate::chunk::{Chunk, Constant};
use harn_lexer::Lexer;
use harn_parser::Parser;

fn compile_source(source: &str) -> Chunk {
    compile_source_with_options(source, CompilerOptions::optimized())
}

fn compile_source_with_options(source: &str, options: CompilerOptions) -> Chunk {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    Compiler::with_options(options).compile(&program).unwrap()
}

fn try_compile(source: &str) -> Result<Chunk, CompileError> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    Compiler::new().compile(&program)
}

#[test]
fn match_list_pattern_rest_must_be_last() {
    let err = try_compile(
        r"pipeline t(task) { match [1, 2, 3] { [...rest, last] -> { log(last) } _ -> {} } }",
    )
    .unwrap_err();
    assert!(err.message.contains("last element"), "{}", err.message);
}

#[test]
fn match_list_pattern_rejects_two_rests() {
    let err = try_compile(
        r"pipeline t(task) { match [1, 2, 3] { [a, ...x, ...y] -> { log(a) } _ -> {} } }",
    )
    .unwrap_err();
    assert!(
        err.message.contains("last element") || err.message.contains("one is allowed"),
        "{}",
        err.message
    );
}

fn disasm_opcodes(disasm: &str) -> Vec<&str> {
    disasm
        .lines()
        .filter_map(|line| {
            line.split_once("] ")
                .and_then(|(_, rest)| rest.split_whitespace().next())
        })
        .collect()
}

fn string_constant_count(chunk: &Chunk, value: &str) -> usize {
    chunk
        .constants
        .iter()
        .filter(|constant| matches!(constant, Constant::String(text) if text == value))
        .count()
}

#[test]
fn test_compile_arithmetic() {
    let chunk = compile_source_with_options(
        "pipeline test(task) { let x = 2 + 3 }",
        CompilerOptions::without_optimizations(),
    );
    assert!(!chunk.code.is_empty());
    assert!(chunk.constants.contains(&Constant::Int(2)));
    assert!(chunk.constants.contains(&Constant::Int(3)));
}

#[test]
fn test_compile_typed_int_loop_ops() {
    let chunk = compile_source(
        "pipeline test(task) {
  var i = 0
  var total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("LESS_INT"));
    assert!(disasm.contains("ADD_INT"));
    assert!(disasm.contains("MUL_INT"));
    assert!(disasm.contains("SUB_INT"));
}

#[test]
fn test_compile_typed_float_ops() {
    let chunk = compile_source(
        "pipeline test(task) {
  let a = 1.0
  let b = 2.0
  let c = a + b
  log(c < 4.0)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("ADD_FLOAT"));
    assert!(disasm.contains("LESS_FLOAT"));
}

#[test]
fn test_compile_typed_equality_ops() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  let a = true
  let b = false
  let left = "a"
  let right = "b"
  log(a == b)
  log(left != right)
}"#,
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("EQUAL_BOOL"));
    assert!(disasm.contains("NOT_EQUAL_STRING"));
}

#[test]
fn test_compile_generic_ops_for_overloaded_or_mixed_cases() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  let left = "a"
  let right = "b"
  let one = 1
  let two = 2.0
  let xs = [1]
  let ys = [2]
  log(left + right)
  log(one + two)
  log(xs + ys)
}"#,
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("ADD"));
    assert!(!disasm.contains("ADD_INT"));
    assert!(!disasm.contains("ADD_FLOAT"));
}

#[test]
fn test_optimizer_folds_scalar_constants() {
    let chunk = compile_source("pipeline test(task) { log(2 + 3 * 4) }");
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk.constants.contains(&Constant::Int(14)));
    assert!(!opcodes.contains(&"ADD_INT"));
    assert!(!opcodes.contains(&"MUL_INT"));
    assert!(!opcodes.contains(&"ADD"));
    assert!(!opcodes.contains(&"MUL"));
}

#[test]
fn test_optimizer_escape_hatch_preserves_unoptimized_bytecode() {
    let chunk = compile_source_with_options(
        "pipeline test(task) { log(2 + 3 * 4) }",
        CompilerOptions::without_optimizations(),
    );
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk.constants.contains(&Constant::Int(2)));
    assert!(chunk.constants.contains(&Constant::Int(3)));
    assert!(chunk.constants.contains(&Constant::Int(4)));
    assert!(opcodes.contains(&"MUL"));
    assert!(opcodes.contains(&"ADD"));
}

#[test]
fn test_optimizer_folds_literal_collections_and_strings() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  log("ha" * 2)
  log([1] + [2, 3])
  log({a: 1} + {b: 2})
}"#,
    );
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(chunk
        .constants
        .contains(&Constant::String("haha".to_string())));
    assert!(!opcodes.contains(&"ADD"));
    assert!(!opcodes.contains(&"MUL"));
}

#[test]
fn test_compiler_reuses_string_constants_within_chunk() {
    let chunk = compile_source(
        r#"pipeline test(task) {
  log("same")
  log("same")
  let row = {status: "same"}
  log(row.status)
}"#,
    );

    assert_eq!(string_constant_count(&chunk, "same"), 1);
    assert_eq!(string_constant_count(&chunk, "status"), 1);
}

#[test]
fn test_compiler_reuses_string_constants_per_nested_chunk() {
    let chunk = compile_source(
        r#"fn inner() {
  log("nested")
  log("nested")
}

pipeline test(task) {
  inner()
}"#,
    );
    let function = chunk
        .functions
        .iter()
        .find(|function| function.name == "inner")
        .expect("inner function should compile");

    assert_eq!(string_constant_count(&function.chunk, "nested"), 1);
}

#[test]
fn test_optimizer_keeps_runtime_erroring_arithmetic_unfolded() {
    let chunk = compile_source("pipeline test(task) { log(1 / 0) }");
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(opcodes.contains(&"DIV_INT"));
}

#[test]
fn test_optimizer_keeps_large_allocations_unfolded() {
    let chunk = compile_source(r#"pipeline test(task) { log("x" * 1000000) }"#);
    let disasm = chunk.disassemble("test");
    let opcodes = disasm_opcodes(&disasm);

    assert!(opcodes.contains(&"MUL"));
}

#[test]
fn test_compile_function_call() {
    let chunk = compile_source("pipeline test(task) { log(42) }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CALL_BUILTIN"));
    assert!(disasm.contains("\"log\""));
}

#[test]
fn test_compile_if_else() {
    let chunk =
        compile_source(r#"pipeline test(task) { if true { log("yes") } else { log("no") } }"#);
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("JUMP_IF_FALSE"));
    assert!(disasm.contains("JUMP"));
}

#[test]
fn test_compile_while() {
    let chunk = compile_source("pipeline test(task) { var i = 0\n while i < 5 { i = i + 1 } }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("JUMP_IF_FALSE"));
    assert!(disasm.contains("JUMP"));
}

#[test]
fn test_compile_locals_to_slots() {
    let chunk = compile_source(
        "pipeline test(task) {
  let a = 1
  var i = 0
  while i < 3 {
    i = i + a
  }
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("DEF_LOCAL_SLOT"));
    assert!(disasm.contains("GET_LOCAL_SLOT"));
    assert!(disasm.contains("SET_LOCAL_SLOT"));
    assert!(!disasm.contains("GET_VAR"));
    assert!(!disasm.contains("SET_VAR"));
}

fn assert_loop_guard_keeps_local_slots(source: &str) {
    let chunk = compile_source(source);
    let disasm = chunk.disassemble("test");
    assert!(
        disasm.contains("GET_LOCAL_SLOT"),
        "expected local-slot reads in disassembly:\n{disasm}"
    );
    assert!(
        disasm.contains("SET_LOCAL_SLOT"),
        "expected local-slot writes in disassembly:\n{disasm}"
    );
    assert!(
        !disasm.contains("GET_VAR"),
        "guarded loop control flow must not make later same-block reads dynamic:\n{disasm}"
    );
    assert!(
        !disasm.contains("SET_VAR"),
        "guarded loop control flow must not make later same-block writes dynamic:\n{disasm}"
    );
}

#[test]
fn loop_guard_break_keeps_later_bindings_in_local_slots() {
    assert_loop_guard_keeps_local_slots(
        r#"pipeline test(task) {
  var index = 0
  while index < 1 {
    let name = "abc"
    if name == "" {
      break
    }
    let value = name + "!"
    index = index + 1
  }
}"#,
    );
}

#[test]
fn loop_guard_continue_keeps_later_bindings_in_local_slots() {
    assert_loop_guard_keeps_local_slots(
        r#"pipeline test(task) {
  var index = 0
  while index < 1 {
    let name = "abc"
    if name == "" {
      continue
    }
    let value = name + "!"
    index = index + 1
  }
}"#,
    );
}

#[test]
fn test_compile_function_params_to_slots() {
    let chunk = compile_source(
        "pipeline test(task) {
  fn add(a, b = 1) {
    return a + b
  }
  log(add(2))
}",
    );
    let disasm = chunk.functions[0].chunk.disassemble("add");
    assert!(disasm.contains("GET_LOCAL_SLOT"));
    assert!(disasm.contains("DEF_LOCAL_SLOT"));
    assert!(!disasm.contains("GET_VAR"));
}

#[test]
fn test_compile_closure() {
    let chunk = compile_source("pipeline test(task) { let f = { x -> x * 2 } }");
    assert!(!chunk.functions.is_empty());
    assert_eq!(
        chunk.functions[0].param_names().collect::<Vec<_>>(),
        vec!["x"]
    );
}

#[test]
fn test_compile_list() {
    let chunk = compile_source("pipeline test(task) { let a = [1, 2, 3] }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("BUILD_LIST"));
}

#[test]
fn test_compile_dict() {
    let chunk = compile_source(r#"pipeline test(task) { let d = {name: "test"} }"#);
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("BUILD_DICT"));
}

#[test]
fn test_disassemble() {
    let chunk = compile_source_with_options(
        "pipeline test(task) { log(2 + 3) }",
        CompilerOptions::without_optimizations(),
    );
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("CONSTANT"));
    assert!(disasm.contains("ADD"));
    assert!(disasm.contains("CALL"));
}

#[test]
fn test_compile_discard_bindings_do_not_define_underscore() {
    let chunk = compile_source(
        r#"
pipeline test(task) {
  let _ = 1
  let [_, keep, _] = [10, 20, 30]
  let {drop: _, keep_dict} = {drop: 1, keep_dict: 2}
  for (_, value) in [pair("left", "right")] {
    log(value)
  }
  log(keep)
  log(keep_dict)
}
"#,
    );

    assert!(
        !chunk.constants.contains(&Constant::String("_".to_string())),
        "discard bindings should not emit a named `_` slot: {:?}",
        chunk.constants
    );
}

/// Regression: an attribute on a top-level declaration must not add a
/// module-level `Op::Pop` in script mode (a file with `fn main`). The
/// script-mode top-level loop pops after any item whose `produces_value`
/// is true; before the fix, a `Node::AttributedDecl` wrapping a `FnDecl`
/// fell through `produces_value`'s `_ => true` catch-all and emitted a
/// `Pop` against an empty operand stack — surfacing only at runtime as
/// "Stack underflow", which broke every `@route`-decorated handler in
/// `harn serve site`.
///
/// `@deprecated` emits no registration bytecode of its own, so the
/// attributed program's module-level op stream must match the bare one
/// exactly. Function bodies live in separate chunks (stored as
/// constants), so the top chunk's disassembly is purely module-level.
#[test]
fn attributed_top_level_fn_does_not_emit_spurious_pop() {
    let bare = compile_source(
        "fn f(x: int) -> int { return x + 1 }\nfn main(harness: Harness) { let _ = 1 }",
    );
    let attributed = compile_source(
        "@deprecated\nfn f(x: int) -> int { return x + 1 }\nfn main(harness: Harness) { let _ = 1 }",
    );

    let pop_count = |chunk: &Chunk| {
        disasm_opcodes(&chunk.disassemble("module"))
            .into_iter()
            .filter(|op| *op == "Pop")
            .count()
    };

    assert_eq!(
        pop_count(&attributed),
        pop_count(&bare),
        "an attributed top-level fn must not add a module-level Pop\n\
         bare:\n{}\nattributed:\n{}",
        bare.disassemble("module"),
        attributed.disassemble("module"),
    );
}

/// #2622: the debug-build balance model classifies straight-line statements
/// exactly. A bare `Op::Closure` leaves one value (`Some(1)`); pairing it
/// with the matching bind (`Op::DefVar`) nets zero (`Some(0)`); and emitting
/// a branch taints the span so the model declines to judge it (`None`).
///
/// The balance model and its assertion are `#[cfg(debug_assertions)]`, so this
/// test (and the miswiring test below) only compile and run in debug builds.
#[cfg(debug_assertions)]
#[test]
fn balance_model_tracks_straight_line_and_declines_branches() {
    let mut chunk = Chunk::new();

    let push_probe = chunk.balance_probe();
    chunk.emit_u16(Op::Closure, 0, 1);
    assert_eq!(chunk.balance_delta_since(push_probe), Some(1));

    // Closure + matching bind is the shape every top-level `fn`/`struct`
    // declaration lowers to — it must net zero.
    let decl_probe = chunk.balance_probe();
    chunk.emit_u16(Op::Closure, 0, 1);
    chunk.emit_u16(Op::DefVar, 0, 1);
    assert_eq!(chunk.balance_delta_since(decl_probe), Some(0));

    // A jump is non-linear: the running sum can't be trusted across it, so
    // the model reports `None` rather than risk a false assertion.
    let branch_probe = chunk.balance_probe();
    let _ = chunk.emit_jump(Op::JumpIfFalse, 1);
    assert_eq!(chunk.balance_delta_since(branch_probe), None);
}

/// #2622: a `produces_value` gap must fail loudly at compile time. We force
/// the value-discarding classification to lie (`true` for a top-level `fn`,
/// which emits a balanced `Closure; DefVar` and leaves nothing to pop) and
/// confirm the balance assertion panics instead of letting the compiler emit
/// the unbalanced `Op::Pop` that #2610 only caught as a runtime underflow.
#[cfg(debug_assertions)]
#[test]
fn miswired_produces_value_trips_balance_assertion() {
    struct ResetGuard;
    impl Drop for ResetGuard {
        fn drop(&mut self) {
            super::state::FORCE_DISCARDED_PRODUCES_VALUE.with(|c| c.set(None));
        }
    }

    let _guard = ResetGuard;
    super::state::FORCE_DISCARDED_PRODUCES_VALUE.with(|c| c.set(Some(true)));

    // Swallow the expected panic's backtrace so it doesn't clutter test output.
    let prior_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_source("fn f() -> int { return 1 }\nfn main(harness: Harness) { let _ = 1 }")
    }));
    std::panic::set_hook(prior_hook);

    assert!(
        result.is_err(),
        "miswiring produces_value to `true` for a balanced top-level decl \
         must trip the #2622 stack-balance assertion",
    );
}

#[test]
fn return_call_inside_handler_does_not_tail_call() {
    let chunk = compile_source(
        r"
fn plain(body) {
  return body()
}

fn guarded(body) {
  try {
    return body()
  } catch (e) {
    return nil
  }
}

fn guarded_retry(body) {
  retry(1) {
    return body()
  }
}

pipeline default() {}
",
    );

    let plain = chunk
        .functions
        .iter()
        .find(|func| func.name == "plain")
        .expect("plain function compiled");
    let plain_disasm = plain.chunk.disassemble("plain");
    assert!(
        plain_disasm.contains("TAIL_CALL"),
        "plain return call should keep the tail-call optimization",
    );

    let guarded = chunk
        .functions
        .iter()
        .find(|func| func.name == "guarded")
        .expect("guarded function compiled");
    let guarded_disasm = guarded.chunk.disassemble("guarded");
    assert!(
        !guarded_disasm.contains("TAIL_CALL"),
        "active try/catch handlers must keep their owning frame alive",
    );
    assert!(
        guarded_disasm.contains("CALL_BUILTIN"),
        "the guarded return expression should still call the callee normally",
    );

    let guarded_retry = chunk
        .functions
        .iter()
        .find(|func| func.name == "guarded_retry")
        .expect("guarded_retry function compiled");
    let guarded_retry_disasm = guarded_retry.chunk.disassemble("guarded_retry");
    assert!(
        !guarded_retry_disasm.contains("TAIL_CALL"),
        "retry handlers must also keep their owning frame alive",
    );
}

#[test]
fn inplace_list_concat_clears_binding_before_add() {
    // `x = x + [i]` on a list compiles to the in-place accumulator form: the
    // binding's reference is cleared (`NIL; SET_LOCAL_SLOT`) before the `ADD`
    // so the runtime concat's `Arc::try_unwrap` extends the existing
    // allocation rather than cloning it (O(n^2) -> O(1) amortized). The signal
    // is a *single* assignment emitting *two* SET_LOCAL_SLOT (clear + store).
    let chunk = compile_source("pipeline t(task) {\n  var x = []\n  x = x + [1]\n}");
    let d = chunk.disassemble("t");
    assert_eq!(
        d.matches("SET_LOCAL_SLOT").count(),
        2,
        "in-place list concat should emit clear-binding + store:\n{d}"
    );
    assert!(
        d.contains("NIL"),
        "expected a NIL clear before the concat:\n{d}"
    );
    assert!(
        !d.contains("ADD_INT"),
        "a list concat must use the generic list ADD, not a scalar op:\n{d}"
    );
}

#[test]
fn inplace_list_concat_compound_assign_form() {
    // `x += [i]` gets the same in-place treatment as `x = x + [i]`.
    let chunk = compile_source("pipeline t(task) {\n  var x = []\n  x += [1]\n}");
    let d = chunk.disassemble("t");
    assert_eq!(
        d.matches("SET_LOCAL_SLOT").count(),
        2,
        "compound `x += [..]` should also emit the in-place form:\n{d}"
    );
    assert!(d.contains("NIL"), "{d}");
}

#[test]
fn inplace_concat_skips_scalar_compound_assign() {
    // `i = i + 1` must NOT take the list peephole: it keeps the specialized
    // ADD_INT fast path and a single store (no clear-binding doubling).
    let chunk = compile_source("pipeline t(task) {\n  var i = 0\n  i = i + 1\n}");
    let d = chunk.disassemble("t");
    assert!(
        d.contains("ADD_INT"),
        "scalar compound-assign must keep the specialized ADD_INT:\n{d}"
    );
    assert_eq!(
        d.matches("SET_LOCAL_SLOT").count(),
        1,
        "scalar assign should emit a single store, not the in-place doubling:\n{d}"
    );
}
