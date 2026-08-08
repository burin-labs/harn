//! Optimizer behavior: it must change speed, never meaning.
//!
//! The differential tests assert optimized and unoptimized builds agree on both
//! results and errors. The rest pin which lambdas get rewritten, that recursive
//! and mutually recursive named functions still resolve afterwards, and that
//! in-place accumulation keeps value semantics (aliases stay immutable, a
//! throwing `+` leaves the binding intact).

use crate::compiler::CompilerOptions;

use super::harness::*;
#[test]
fn optimizer_differential_success_programs_match() {
    let programs = [
        r#"pipeline test(harness: Harness, task) {
  harness.stdio.println(2 + 3 * 4)
  harness.stdio.println("ha" * 2)
  harness.stdio.println(([1] + [2, 3])[2])
  harness.stdio.println(({a: 1} + {b: 2}).b)
  harness.stdio.println((true && false) || !false)
}"#,
        r"pipeline test(harness: Harness, task) {
  fn add(a: int, b: int = 4) {
    return a + b
  }
  const base = 3
  harness.stdio.println(add(base))
  harness.stdio.println(add(1 + 1, 2 + 2))
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
    let source = "pipeline test(harness: Harness, task) { harness.stdio.println(1 / 0) }";
    let optimized =
        run_harn_result_display_with_options(source, CompilerOptions::optimized()).unwrap_err();
    let unoptimized =
        run_harn_result_display_with_options(source, CompilerOptions::without_optimizations())
            .unwrap_err();

    assert_eq!(optimized, unoptimized);
}

#[test]
fn inline_arithmetic_lambda_map_filter_optimization_path() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            const evens = [1, 2, 3, 4, 5, 6].filter({ x -> x % 2 == 0 })
            const doubled = evens.map({ x -> x * 2 })
            harness.stdio.log(doubled)
        }",
    );
    assert_eq!(out, "[harn] [4, 8, 12]\n");
}

#[test]
fn self_recursive_named_fn_still_resolves_after_optimization() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            fn fact(n) {
                if n <= 1 { return 1 }
                return n * fact(n - 1)
            }
            harness.stdio.log(fact(6))
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
        r"pipeline default(harness: Harness, task) {
            fn is_even(n) {
                if n == 0 { return true }
                return is_odd(n - 1)
            }
            fn is_odd(n) {
                if n == 0 { return false }
                return is_even(n - 1)
            }
            harness.stdio.log(is_even(4))
            harness.stdio.log(is_even(5))
        }",
    );
    assert_eq!(out, "[harn] true\n[harn] false\n");
}

#[test]
fn anonymous_lambda_calling_sibling_fn_via_call_builtin_flags() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            fn helper(x) { return x + 100 }
            const r = [1, 2, 3].map({ v -> helper(v) })
            harness.stdio.log(r)
        }",
    );
    assert_eq!(out, "[harn] [101, 102, 103]\n");
}

#[test]
fn anonymous_lambda_with_get_var_capture_flags() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            const bonus = 10
            const r = [1, 2, 3].map({ v -> v + bonus })
            harness.stdio.log(r)
        }",
    );
    assert_eq!(out, "[harn] [11, 12, 13]\n");
}

#[test]
fn pure_lambda_inside_pipeline_with_unrelated_locals_skips_walk() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            fn helper_a(x) { return x + 1 }
            fn helper_b(x) { return x + 2 }
            const r = [10, 20, 30].map({ v -> v * 2 })
            harness.stdio.log(r)
            harness.stdio.log(helper_a(0))
            harness.stdio.log(helper_b(0))
        }",
    );
    assert_eq!(out, "[harn] [20, 40, 60]\n[harn] 1\n[harn] 2\n");
}

#[test]
fn nested_map_lambdas_skip_walk_independently() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            const grid = [[1, 2], [3, 4]]
            const r = grid.map({ row -> row.map({ x -> x * 10 }) })
            harness.stdio.log(r)
        }",
    );
    assert_eq!(out, "[harn] [[10, 20], [30, 40]]\n");
}

#[test]
fn typed_param_lambda_uses_check_type_and_walks() {
    let out = run_vm(
        r"pipeline default(harness: Harness, task) {
            const r = [1, 2, 3].map({ v: int -> v + 1 })
            harness.stdio.log(r)
        }",
    );
    assert_eq!(out, "[harn] [2, 3, 4]\n");
}

/// Regression: a `let` inferred `int` from its initializer but later reassigned
/// through an `any`-typed value of a different primitive must not be specialized
/// into a typed opcode (`AddInt`), which would hard-error at runtime on a
/// program the generic path runs correctly. The optimized result must match the
/// unoptimized one exactly. (Previously the optimizer threw
/// "Typed int add expected int operands, got int and float".)
#[test]
fn var_reassigned_via_any_matches_unoptimized() {
    let source = r#"pipeline default(harness: Harness, task) {
  let x = 0
  let sum = 0
  let i = 0
  const cell = harness.runtime.shared_cell("k", 2.5)
  while i < 3 {
    sum = sum + x
    if i == 0 { x = harness.runtime.shared_get(cell) }
    i = i + 1
  }
  harness.stdio.log("${sum}")
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
    let source = r#"pipeline default(harness: Harness, task) {
  let sum = 0
  const cell = harness.runtime.shared_cell("k", 2.5)
  for n in [1, 2, 3] {
    sum = sum + n
    n = harness.runtime.shared_get(cell)
    sum = sum + n
  }
  harness.stdio.log("${sum}")
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
    let source = r#"pipeline default(harness: Harness, task) {
  let i = 0
  let total = 0
  while i < 10 {
    total = total + (i + 3) * 2 - 1
    i = i + 1
  }
  harness.stdio.log("${total}")
}"#;
    let (out, _) = run_harn(source);
    assert_eq!(out.trim_end(), "[harn] 140");
}

#[test]
fn inplace_concat_untyped_accumulator_builds_correctly() {
    // An accumulator whose static type is unknown (`any`-returning seed) goes
    // through the fused `ConcatAssignLocal` opcode, which gates the in-place
    // take on the runtime value. The accumulated list must be correct.
    let source = r#"
fn seed() -> any { return [] }
pipeline t(harness: Harness, task) {
  let x = seed()
  for i in [1, 2, 3] {
    x = x + [i]
  }
  let y = seed()
  for i in [4, 5] {
    y += [i]
  }
  harness.stdio.log("${x} ${y}")
}"#;
    assert_eq!(run_output(source), "[harn] [1, 2, 3] [4, 5]");
}

#[test]
fn list_appending_assign_preserves_alias_immutability() {
    // The compiler lowers `x = x.appending(v)` through the same fused concat opcode
    // as `x = x + [v]`. Rebinding `x` must still leave an existing alias `y`
    // pointing at the original immutable list.
    let source = r#"
pipeline t(harness: Harness, task) {
  let x = []
  x = x.appending(1)
  const y = x
  x = x.appending(2)
  harness.stdio.log("${x} ${y}")
}"#;
    assert_eq!(run_output(source), "[harn] [1, 2] [1]");
}

#[test]
fn inplace_concat_preserves_binding_when_add_throws() {
    // The fused opcode only takes the slot in place for List/Dict values. A
    // scalar accumulator hit with an incompatible `+=` throws, and the binding
    // must retain its previous value (it was cloned, not taken) rather than be
    // left as the placeholder.
    let source = r#"
pipeline t(harness: Harness, task) {
  let x = 5
  try {
    x += [1]
  } catch (e) {
    harness.stdio.log("caught")
  }
  harness.stdio.log("${x}")
}"#;
    assert_eq!(run_output(source), "[harn] caught\n[harn] 5");
}
