//! Runtime guard / fallback behavior for typed fast-path opcodes.
//!
//! The compiler emits typed opcodes (`AddInt`, `LessInt`, `EqualString`, …)
//! from a *static* type guess. A guess can be wrong at runtime, so these opcodes
//! guard their operands and fall back to the exact generic result the
//! unoptimized build produces instead of hard-erroring. Every test asserts
//! `optimized == unoptimized`.
//!
//! Since harn#6252 an annotated `let` / `const` is checked at the binding site,
//! so an annotation can no longer be the *source* of the drift: the initializer
//! is rejected before any typed opcode sees it. Since harn#6267 a declared
//! `int` parameter rejects a float the same way, so the native parameter guard
//! is no longer a drift route either — see
//! `annotated_binding_initializer_is_rejected_not_absorbed` and
//! `typed_param_fed_dynamic_float_is_rejected`.

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
fn typed_param_fed_dynamic_float_is_rejected() {
    // `n: int` used to accept a dynamic float under numeric_compat and then
    // rely on `MulInt` falling back. The parameter guard now matches the
    // kernel rule, so the float never reaches the body.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(harness: Harness, task: unknown) {
  fn f(n: int) { return n * 2 }
  const cell = harness.runtime.shared_cell("k", 2.5)
  harness.stdio.log("${f(harness.runtime.shared_get(cell))}")
}"#,
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("parameter 'n'") && error.contains("expected int"),
        "expected the parameter-site type error, got: {error}"
    );
}

#[test]
fn annotated_binding_initializer_is_rejected_not_absorbed() {
    // `const x: int = <dynamic float>` used to reach `x + 1` as a float and rely
    // on `AddInt` falling back. The annotation is now checked where it is
    // written, so the float never becomes an `int`-declared binding at all.
    //
    // Both builds must agree on the rejection: the optimizer must not be the
    // difference between a caught and an uncaught type error.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(harness: Harness, task: unknown) {
  const cell = harness.runtime.shared_cell("k", 2.5)
  const x: int = harness.runtime.shared_get(cell)
  harness.stdio.log("${x + 1}")
}"#,
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("binding `x`") && error.contains("expects int"),
        "expected the binding-site type error, got: {error}"
    );
}

#[test]
fn annotated_mutable_binding_is_checked_like_an_immutable_one() {
    // `let` and `const` are the same binding site for this purpose. The
    // monomorphic-binding analysis trusts an annotated, never-reassigned `let`
    // for typed-opcode specialization; the binding check is what now makes that
    // trust sound rather than merely convenient.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(harness: Harness, task: unknown) {
  const cell = harness.runtime.shared_cell("k", 4.5)
  let x: int = harness.runtime.shared_get(cell)
  harness.stdio.log("${x - 1}")
}"#,
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("binding `x`") && error.contains("expects int"),
        "expected the binding-site type error, got: {error}"
    );
}

#[test]
fn typed_comparison_fed_dynamic_float_is_rejected() {
    // Same tightening as `typed_param_fed_dynamic_float_is_rejected`: a float
    // argument to an `int` parameter is rejected at the call, so `LessInt`
    // never sees a drifted operand from this route.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(harness: Harness, task: unknown) {
  fn under(n: int) { return n < 3 }
  const cell = harness.runtime.shared_cell("k", 2.5)
  harness.stdio.log("${under(harness.runtime.shared_get(cell))}")
}"#,
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("parameter 'n'") && error.contains("expected int"),
        "expected the parameter-site type error, got: {error}"
    );
}

#[test]
fn typed_string_equality_fed_dynamic_int_falls_back() {
    // `EqualString` guarded: comparing a binding that the optimizer inferred as
    // a string but that actually holds an int is `false` generically, not a
    // throw.
    //
    // The drift enters through reassignment. Neither annotation site can produce
    // it any more — a declared `string` binding rejects the int where it is
    // written, and a declared `string` parameter rejects it at the call — but an
    // *inferred* type fact is still only a guess, which is what this guards.
    let result = assert_opt_matches_unopt(
        r#"pipeline default(harness: Harness, task: unknown) {
  const cell = harness.runtime.shared_cell("k", 7)
  let s = "7"
  s = harness.runtime.shared_get(cell)
  harness.stdio.log("${s == "7"}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] false");
}

#[test]
fn genuinely_incompatible_operands_error_identically() {
    // A declared `int` holding a string is now stopped at the binding rather
    // than at `x + 1`, but the property under test is unchanged and is the one
    // that matters: both builds fail, identically. No optimized-only crash and
    // no silent wrong answer.
    let optimized = run(
        r#"pipeline default(harness: Harness, task: unknown) {
  const cell = harness.runtime.shared_cell("k", "hi")
  const x: int = harness.runtime.shared_get(cell)
  harness.stdio.log("${x + 1}")
}"#,
        CompilerOptions::optimized(),
    );
    let baseline = run(
        r#"pipeline default(harness: Harness, task: unknown) {
  const cell = harness.runtime.shared_cell("k", "hi")
  const x: int = harness.runtime.shared_get(cell)
  harness.stdio.log("${x + 1}")
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
        r#"pipeline default(harness: Harness, task: unknown) {
  let i = 0
  let total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
  harness.stdio.log("${total}")
}"#,
    );
    assert_eq!(result.unwrap(), "[harn] 140");
}
