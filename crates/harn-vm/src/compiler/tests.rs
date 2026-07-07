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
fn oversized_function_chunk_is_a_compile_error_not_a_miscompile() {
    // Jump operands are u16 chunk offsets; a body that compiles past
    // 64 KiB used to silently truncate jump targets (`loop_start as
    // u16`, `patch_jump`'s `code.len() as u16`) and jump somewhere
    // wild at runtime. It must be a compile error instead.
    let mut body = String::new();
    for i in 0..6000 {
        // Each iteration emits a conditional, so the chunk accumulates
        // patched jumps on both sides of the 64 KiB boundary.
        body.push_str(&format!("  if task == {i} {{ log({i}) }}\n"));
    }
    let source = format!("fn huge(task: int) {{\n{body}}}\npipeline t(task) {{ huge(1) }}");
    let err = try_compile(&source).unwrap_err();
    assert!(
        err.message.contains("64 KiB") && err.message.contains("huge"),
        "{}",
        err.message
    );
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
        "pipeline test(task) { const x = 2 + 3 }",
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
  let i = 0
  let total = 0
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
  const a = 1.0
  const b = 2.0
  const c = a + b
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
  const a = true
  const b = false
  const left = "a"
  const right = "b"
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
  const left = "a"
  const right = "b"
  const one = 1
  const two = 2.0
  const xs = [1]
  const ys = [2]
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
fn monomorphic_var_keeps_typed_int_ops() {
    // A `var` only ever reassigned through int-typed values is provably
    // monomorphic, so its arithmetic keeps the typed fast path even when the
    // use precedes the reassignment in source order.
    let chunk = compile_source(
        "pipeline test(task) {
  let x = 0
  let i = 0
  while i < 3 {
    log(x + 1)
    x = x + 2
    i = i + 1
  }
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(
        disasm.contains("ADD_INT"),
        "expected typed add, got:\n{disasm}"
    );
    assert!(disasm.contains("LESS_INT"));
}

#[test]
fn polymorphic_var_reassigned_from_dynamic_falls_back_to_generic() {
    // `x` is inferred `int` from its initializer but later reassigned from a
    // statically-unknown (`any`) call result, so its runtime primitive type can
    // change. Committing `x + 1` to ADD_INT would be unsound, so the compiler
    // must keep the generic adaptive ADD. (No int counter here, so ADD_INT
    // appearing at all would mean `x` was wrongly specialized.)
    let chunk = compile_source(
        "pipeline test(task) {
  let x = 0
  const cell = shared_cell(\"k\", 2.5)
  log(x + 1)
  x = shared_get(cell)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(
        disasm.contains("ADD"),
        "expected generic add, got:\n{disasm}"
    );
    assert!(
        !disasm.contains("ADD_INT"),
        "polymorphic var must not specialize, got:\n{disasm}"
    );
}

#[test]
fn polymorphic_var_demotes_dependent_sibling() {
    // `sum` only ever takes `sum + x`, but `x` is polymorphic, so `sum`'s
    // primitive type is not provable either — the fixpoint must demote both.
    let chunk = compile_source(
        "pipeline test(task) {
  let x = 0
  let sum = 0
  const cell = shared_cell(\"k\", 2.5)
  sum = sum + x
  x = shared_get(cell)
  log(sum)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(
        !disasm.contains("ADD_INT"),
        "x and its dependent sum must both fall back, got:\n{disasm}"
    );
}

#[test]
fn for_item_reassigned_from_dynamic_falls_back_to_generic() {
    // A `for`-item binding is reassignable per iteration; reassigning it from an
    // `any` value makes its primitive type unprovable, so arithmetic on it must
    // stay generic.
    let chunk = compile_source(
        "pipeline test(task) {
  let sum = 0
  const cell = shared_cell(\"k\", 2.5)
  for n in [1, 2, 3] {
    sum = sum + n
    n = shared_get(cell)
  }
  log(sum)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(
        !disasm.contains("ADD_INT"),
        "reassigned for-item must not specialize, got:\n{disasm}"
    );
}

#[test]
fn for_item_never_reassigned_keeps_typed_ops() {
    // The common case: a `for`-item that is never reassigned stays on the typed
    // fast path.
    let chunk = compile_source(
        "pipeline test(task) {
  let sum = 0
  for n in [1, 2, 3] {
    sum = sum + n
  }
  log(sum)
}",
    );
    let disasm = chunk.disassemble("test");
    assert!(
        disasm.contains("ADD_INT"),
        "unreassigned for-item should specialize, got:\n{disasm}"
    );
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
  const row = {status: "same"}
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
    let chunk = compile_source("pipeline test(task) { let i = 0\n while i < 5 { i = i + 1 } }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("JUMP_IF_FALSE"));
    assert!(disasm.contains("JUMP"));
}

#[test]
fn test_compile_locals_to_slots() {
    let chunk = compile_source(
        "pipeline test(task) {
  const a = 1
  let i = 0
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
  let index = 0
  while index < 1 {
    const name = "abc"
    if name == "" {
      break
    }
    const value = name + "!"
    index = index + 1
  }
}"#,
    );
}

#[test]
fn loop_guard_continue_keeps_later_bindings_in_local_slots() {
    assert_loop_guard_keeps_local_slots(
        r#"pipeline test(task) {
  let index = 0
  while index < 1 {
    const name = "abc"
    if name == "" {
      continue
    }
    const value = name + "!"
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
    let chunk = compile_source("pipeline test(task) { const f = { x -> x * 2 } }");
    assert!(!chunk.functions.is_empty());
    assert_eq!(
        chunk.functions[0].param_names().collect::<Vec<_>>(),
        vec!["x"]
    );
}

#[test]
fn test_compile_list() {
    let chunk = compile_source("pipeline test(task) { const a = [1, 2, 3] }");
    let disasm = chunk.disassemble("test");
    assert!(disasm.contains("BUILD_LIST"));
}

#[test]
fn test_compile_dict() {
    let chunk = compile_source(r#"pipeline test(task) { const d = {name: "test"} }"#);
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
  const _ = 1
  const [_, keep, _] = [10, 20, 30]
  const {drop: _, keep_dict} = {drop: 1, keep_dict: 2}
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
        "fn f(x: int) -> int { return x + 1 }\nfn main(harness: Harness) { const _ = 1 }",
    );
    let attributed = compile_source(
        "@deprecated\nfn f(x: int) -> int { return x + 1 }\nfn main(harness: Harness) { const _ = 1 }",
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
        compile_source("fn f() -> int { return 1 }\nfn main(harness: Harness) { const _ = 1 }")
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
fn inplace_list_concat_uses_fused_opcode() {
    // `x = x + [i]` on a local list accumulator compiles to the single fused
    // `CONCAT_ASSIGN_LOCAL` opcode. At runtime it takes the slot's value in
    // place before the concat so `Arc::try_unwrap` extends the existing
    // allocation rather than cloning it (O(n^2) -> O(1) amortized).
    let chunk = compile_source("pipeline t(task) {\n  let x = []\n  x = x + [1]\n}");
    let d = chunk.disassemble("t");
    assert!(
        d.contains("CONCAT_ASSIGN_LOCAL"),
        "in-place list concat should emit the fused opcode:\n{d}"
    );
    assert!(
        !d.contains("ADD_INT"),
        "a list concat must not use a scalar op:\n{d}"
    );
}

#[test]
fn list_push_assign_uses_fused_concat_opcode() {
    // `x = x.push(i)` is the method spelling of an immutable list append, so a
    // local list accumulator should use the same fused concat opcode as
    // `x = x + [i]` instead of dispatching through the cloning list method.
    let chunk = compile_source("pipeline t(task) {\n  let x = []\n  x = x.push(1)\n}");
    let d = chunk.disassemble("t");
    assert!(
        d.contains("CONCAT_ASSIGN_LOCAL"),
        "list push assignment should emit the fused opcode:\n{d}"
    );
    assert!(
        !d.contains("METHOD_CALL"),
        "optimized list push assignment should skip method dispatch:\n{d}"
    );
}

#[test]
fn inplace_list_concat_compound_assign_form() {
    // `x += [i]` gets the same fused opcode as `x = x + [i]`.
    let chunk = compile_source("pipeline t(task) {\n  let x = []\n  x += [1]\n}");
    let d = chunk.disassemble("t");
    assert!(
        d.contains("CONCAT_ASSIGN_LOCAL"),
        "compound `x += [..]` should also emit the fused opcode:\n{d}"
    );
}

#[test]
fn inplace_concat_fires_for_untyped_local_accumulator() {
    // The fused opcode is gated on the *runtime* value, so an accumulator
    // whose static type is unknown (`any`-returning helper) still gets the
    // in-place path — the gap the compile-time-typed peephole could not close.
    let chunk = compile_source(
        "fn seed() -> any { return [] }\npipeline t(task) {\n  let x = seed()\n  x = x + [1]\n}",
    );
    let d = chunk.disassemble("t");
    assert!(
        d.contains("CONCAT_ASSIGN_LOCAL"),
        "untyped local accumulator should still use the fused opcode:\n{d}"
    );
}

#[test]
fn inplace_concat_skips_scalar_compound_assign() {
    // `i = i + 1` must NOT take the list peephole: it keeps the specialized
    // ADD_INT fast path and a single store (no clear-binding doubling).
    let chunk = compile_source("pipeline t(task) {\n  let i = 0\n  i = i + 1\n}");
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

#[test]
fn local_property_assignment_uses_slot_opcode() {
    let chunk = compile_source("pipeline t(task) {\n  let out = {}\n  out.a = 1\n}");
    let d = chunk.disassemble("t");
    let opcodes = disasm_opcodes(&d);
    assert!(
        opcodes.contains(&"SET_LOCAL_SLOT_PROPERTY"),
        "local property assignment should store by slot:\n{d}"
    );
    assert!(
        !opcodes.contains(&"SET_PROPERTY"),
        "slot-resolved local property assignment should skip by-name store:\n{d}"
    );
}

#[test]
fn local_subscript_assignment_uses_slot_opcode() {
    let chunk = compile_source("pipeline t(task) {\n  let out = {}\n  out[\"a\"] = 1\n}");
    let d = chunk.disassemble("t");
    let opcodes = disasm_opcodes(&d);
    assert!(
        opcodes.contains(&"SET_LOCAL_SLOT_SUBSCRIPT"),
        "local subscript assignment should store by slot:\n{d}"
    );
    assert!(
        !opcodes.contains(&"SET_SUBSCRIPT"),
        "slot-resolved local subscript assignment should skip by-name store:\n{d}"
    );
}

#[test]
fn nonlocal_subscript_assignment_keeps_by_name_opcode() {
    let chunk = compile_source("let out = {}\npipeline t(task) {\n  out[\"a\"] = 1\n}");
    let d = chunk.disassemble("t");
    let opcodes = disasm_opcodes(&d);
    assert!(
        opcodes.contains(&"SET_SUBSCRIPT"),
        "non-local subscript assignment must keep env-aware store:\n{d}"
    );
    assert!(
        !opcodes.contains(&"SET_LOCAL_SLOT_SUBSCRIPT"),
        "non-local assignment cannot be lowered to a frame-local slot:\n{d}"
    );
}
