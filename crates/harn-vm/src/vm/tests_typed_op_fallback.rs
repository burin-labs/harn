//! Runtime guard / fallback behavior for typed fast-path opcodes.
//!
//! The compiler emits typed opcodes (`AddInt`, `LessInt`, `EqualString`, …)
//! from a *static* type guess. A guess can be wrong at runtime — an `any`-typed
//! value flowing through a typed parameter or an annotated binding initializer
//! is not runtime-checked, so the operand may be a different primitive than the
//! annotation claims. These opcodes therefore guard their operands and fall back
//! to the exact generic result the unoptimized build produces, instead of
//! hard-erroring. Every test asserts `optimized == unoptimized`.

use crate::compiler::{Compiler, CompilerOptions};
use crate::stdlib::register_vm_stdlib;
use crate::vm::Vm;
use harn_lexer::Lexer;
use harn_parser::Parser;

/// Compile + run `source` under the given options, returning either the
/// captured stdout (Ok) or the runtime/compile error rendered as a string
/// (Err). Single-threaded current-thread runtime; fully in-process.
fn run(source: &str, options: CompilerOptions) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let tokens = Lexer::new(source).tokenize().map_err(|e| e.to_string())?;
                let program = Parser::new(tokens).parse().map_err(|e| e.to_string())?;
                let chunk = Compiler::with_options(options)
                    .compile(&program)
                    .map_err(|e| e.to_string())?;
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.map_err(|e| e.to_string())?;
                Ok(vm.output().trim_end().to_string())
            })
            .await
    })
}

/// Assert the optimized and unoptimized builds agree, and return the shared
/// result for the caller to pin down further.
#[track_caller]
fn assert_opt_matches_unopt(source: &str) -> Result<String, String> {
    let optimized = run(source, CompilerOptions::optimized());
    let baseline = run(source, CompilerOptions::without_optimizations());
    assert_eq!(
        optimized, baseline,
        "optimized build must match the unoptimized (generic) build"
    );
    optimized
}

#[test]
fn typed_param_fed_dynamic_float_falls_back() {
    // `n: int` is a static guess; the `any` argument is actually a float. The
    // typed `MulInt` must fall back to generic multiply, not throw.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  fn f(n: int) { return n * 2 }
  const cell = shared_cell("k", 2.5)
  log("${f(shared_get(cell))}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] 5.0");
}

#[test]
fn annotated_let_initializer_from_dynamic_float_falls_back() {
    // `let x: int = <any float>` — the annotation is not runtime-enforced, so
    // the initializer is really a float. `x + 1` must fall back to generic add.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  const cell = shared_cell("k", 2.5)
  const x: int = shared_get(cell)
  log("${x + 1}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] 3.5");
}

#[test]
fn annotated_var_initializer_from_dynamic_float_falls_back() {
    // The `var` analogue: an annotated, never-reassigned `var` whose initializer
    // is a dynamic float. (The monomorphic-binding analysis trusts it because it
    // is never reassigned; the runtime guard is what keeps it sound.)
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  const cell = shared_cell("k", 4.5)
  let x: int = shared_get(cell)
  log("${x - 1}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] 3.5");
}

#[test]
fn typed_comparison_fed_dynamic_float_falls_back() {
    // A typed `LessInt` fed a float operand must fall back to the generic
    // comparison rather than throwing.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  const cell = shared_cell("k", 2.5)
  const x: int = shared_get(cell)
  log("${x < 3}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] true");
}

#[test]
fn typed_string_equality_fed_dynamic_int_falls_back() {
    // `EqualString` guarded: comparing a declared-string binding that actually
    // holds an int against a string literal is `false` generically, not a throw.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  const cell = shared_cell("k", 7)
  const s: string = shared_get(cell)
  log("${s == "7"}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] false");
}

#[test]
fn genuinely_incompatible_operands_error_identically() {
    // The fallback is to *generic* semantics, which still rejects truly
    // incompatible operands (int + string) — and with the same error in both
    // builds, so no optimized-only crash and no silent wrong answer.
    let optimized = run(
        r#"pipeline default(task) {
  const cell = shared_cell("k", "hi")
  const x: int = shared_get(cell)
  log("${x + 1}")
}"#,
        CompilerOptions::optimized(),
    );
    let baseline = run(
        r#"pipeline default(task) {
  const cell = shared_cell("k", "hi")
  const x: int = shared_get(cell)
  log("${x + 1}")
}"#,
        CompilerOptions::without_optimizations(),
    );
    assert!(optimized.is_err(), "int + string must still error");
    assert_eq!(optimized, baseline, "error must match the generic build");
}

#[test]
fn monomorphic_fast_path_still_correct() {
    // The hot path (operands match the static guess) is unaffected.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(task) {
  let i = 0
  let total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
  log("${total}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] 140");
}
