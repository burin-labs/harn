//! End-to-end differential tests: compile real Harn source to native code and
//! assert the JIT agrees with the reference interpreter (and with hand-computed
//! expectations) across a grid of inputs.
//!
//! These are fully in-process and deterministic — no wall-clock, no threads,
//! no external toolchain.

use harn_codegen::{
    analyze_named, evaluate, jit_compile, CodegenError, DeoptReason, EvalError, NativeOutcome,
    NativeTrap, ScalarValue,
};

use ScalarValue::{Bool, Float, Int};

/// Compare two scalar values, treating floats bit-for-bit so NaN results
/// (which are never `==` under `PartialEq`) compare equal when identical.
fn same_value(a: ScalarValue, b: ScalarValue) -> bool {
    match (a, b) {
        (Float(x), Float(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// Compare two outcomes: values bit-for-bit (per [`same_value`]), deopts by
/// reason. The JIT and reference interpreter must agree on both.
fn same(a: NativeOutcome, b: NativeOutcome) -> bool {
    match (a, b) {
        (NativeOutcome::Value(x), NativeOutcome::Value(y)) => same_value(x, y),
        (NativeOutcome::Deopt(x), NativeOutcome::Deopt(y)) => x == y,
        _ => false,
    }
}

/// Compile `function` from `source`, then assert the JIT and the reference
/// interpreter agree on every input row.
fn check(source: &str, function: &str, rows: &[Vec<ScalarValue>]) {
    let scalar = analyze_named(source, function)
        .unwrap_or_else(|e| panic!("`{function}` should be scalar-eligible: {e}"));
    let native = jit_compile(&scalar).expect("jit compile");

    for args in rows {
        let jit = native.call(args);
        let reference = evaluate(&scalar, args);
        match (jit, reference) {
            (Ok(j), Ok(r)) => assert!(
                same(j, r),
                "jit={j:?} reference={r:?} for {function}{args:?}"
            ),
            (Err(jt), Err(EvalError::Trap(rt))) => {
                assert_eq!(jt, rt, "trap mismatch for {function}{args:?}");
            }
            (j, r) => panic!("jit/reference disagree for {function}{args:?}: {j:?} vs {r:?}"),
        }
    }
}

fn ints(values: &[i64]) -> Vec<Vec<ScalarValue>> {
    values.iter().map(|&n| vec![Int(n)]).collect()
}

fn int_pairs(values: &[(i64, i64)]) -> Vec<Vec<ScalarValue>> {
    values.iter().map(|&(a, b)| vec![Int(a), Int(b)]).collect()
}

const INT_SAMPLES: &[i64] = &[0, 1, -1, 2, -7, 42, 1000, i64::MIN, i64::MAX, -123_456];

#[test]
fn integer_arithmetic_matches_interpreter() {
    let pairs: Vec<(i64, i64)> = [
        (0, 0),
        (1, 2),
        (-3, 9),
        (i64::MAX, 1),
        (i64::MIN, -1),
        (7, -2),
        (-7, 2),
        (i64::MIN, 1),
        (100, 7),
    ]
    .to_vec();

    for (name, op) in [
        ("add", "+"),
        ("sub", "-"),
        ("mul", "*"),
        ("idiv", "/"),
        ("imod", "%"),
    ] {
        let src = format!("fn {name}(a: int, b: int) -> int {{ return a {op} b }}");
        check(&src, name, &int_pairs(&pairs));
    }
}

#[test]
fn integer_overflow_deopts_not_wraps() {
    // The Harn VM promotes an overflowing +/-/* to float; a monomorphic int
    // kernel cannot, so the JIT and the reference interpreter both deopt
    // (rather than silently wrapping, which would disagree with the VM).
    // Each row is `(fn name, operator, overflowing operand pairs)`.
    type OverflowCase = (&'static str, &'static str, &'static [(i64, i64)]);
    let overflow_cases: &[OverflowCase] = &[
        (
            "add",
            "+",
            &[(i64::MAX, 1), (i64::MIN, -1), (i64::MAX, i64::MAX)],
        ),
        (
            "sub",
            "-",
            &[(i64::MIN, 1), (i64::MAX, -1), (i64::MIN, i64::MAX)],
        ),
        (
            "mul",
            "*",
            &[(i64::MAX, 2), (i64::MIN, -1), (i64::MAX, i64::MAX)],
        ),
    ];
    for (name, op, rows) in overflow_cases {
        let src = format!("fn {name}(a: int, b: int) -> int {{ return a {op} b }}");
        let scalar = analyze_named(&src, name).unwrap();
        let native = jit_compile(&scalar).unwrap();
        for &(a, b) in *rows {
            let args = [Int(a), Int(b)];
            let deopt = NativeOutcome::Deopt(DeoptReason::IntegerOverflow);
            assert_eq!(native.call(&args), Ok(deopt), "jit {name}({a}, {b})");
            assert_eq!(evaluate(&scalar, &args), Ok(deopt), "ref {name}({a}, {b})");
        }
        // A non-overflowing input on the same function still returns a value.
        assert!(matches!(
            native.call(&[Int(2), Int(3)]),
            Ok(NativeOutcome::Value(_))
        ));
    }

    // Unary negation of i64::MIN overflows and must deopt too.
    let scalar = analyze_named("fn neg(a: int) -> int { return -a }", "neg").unwrap();
    let native = jit_compile(&scalar).unwrap();
    let deopt = NativeOutcome::Deopt(DeoptReason::IntegerOverflow);
    assert_eq!(native.call(&[Int(i64::MIN)]), Ok(deopt));
    assert_eq!(evaluate(&scalar, &[Int(i64::MIN)]), Ok(deopt));
    assert_eq!(native.call(&[Int(7)]), Ok(NativeOutcome::Value(Int(-7))));
}

#[test]
fn integer_min_div_neg_one_does_not_trap_or_deopt() {
    // i64::MIN / -1 overflows in two's complement, but the VM's int division
    // uses wrapping_div (-> i64::MIN) / wrapping_rem (-> 0) rather than
    // promoting — so the JIT must wrap, not deopt, and not hardware-trap.
    let div = analyze_named("fn d(a: int, b: int) -> int { return a / b }", "d").unwrap();
    let native = jit_compile(&div).unwrap();
    assert_eq!(
        native.call(&[Int(i64::MIN), Int(-1)]).unwrap(),
        NativeOutcome::Value(Int(i64::MIN))
    );

    let rem = analyze_named("fn m(a: int, b: int) -> int { return a % b }", "m").unwrap();
    let native = jit_compile(&rem).unwrap();
    assert_eq!(
        native.call(&[Int(i64::MIN), Int(-1)]).unwrap(),
        NativeOutcome::Value(Int(0))
    );
}

#[test]
fn divide_by_zero_traps_in_jit_and_interpreter() {
    for op in ["/", "%"] {
        let src = format!("fn d(a: int, b: int) -> int {{ return a {op} b }}");
        let scalar = analyze_named(&src, "d").unwrap();
        let native = jit_compile(&scalar).unwrap();
        assert_eq!(
            native.call(&[Int(5), Int(0)]),
            Err(NativeTrap::DivideByZero)
        );
        assert_eq!(
            evaluate(&scalar, &[Int(5), Int(0)]),
            Err(EvalError::Trap(NativeTrap::DivideByZero))
        );
        // A subsequent non-trapping call on the same compiled function still
        // works (the trap flag is reset on every entry).
        let ok = native.call(&[Int(6), Int(2)]).unwrap();
        assert_eq!(ok, evaluate(&scalar, &[Int(6), Int(2)]).unwrap());
    }
}

#[test]
fn unary_and_comparisons() {
    check(
        "fn neg(a: int) -> int { return -a }",
        "neg",
        &ints(INT_SAMPLES),
    );
    for (name, op) in [
        ("lt", "<"),
        ("gt", ">"),
        ("le", "<="),
        ("ge", ">="),
        ("eq", "=="),
        ("ne", "!="),
    ] {
        let src = format!("fn {name}(a: int, b: int) -> bool {{ return a {op} b }}");
        check(
            &src,
            name,
            &int_pairs(&[(1, 2), (2, 1), (3, 3), (-1, -1), (i64::MIN, i64::MAX)]),
        );
    }
}

#[test]
fn float_arithmetic_and_special_values() {
    let rows: Vec<Vec<ScalarValue>> = [
        (1.5, 2.25),
        (-3.0, 0.0),
        (1.0, 0.0),  // -> +inf
        (-1.0, 0.0), // -> -inf
        (0.0, 0.0),  // -> NaN for div
        (f64::INFINITY, 1.0),
        (f64::NAN, 1.0),
        (2.0, 0.5),
    ]
    .iter()
    .map(|&(a, b)| vec![Float(a), Float(b)])
    .collect();

    for (name, op) in [("add", "+"), ("sub", "-"), ("mul", "*"), ("fdiv", "/")] {
        let src = format!("fn {name}(a: float, b: float) -> float {{ return a {op} b }}");
        check(&src, name, &rows);
    }
}

#[test]
fn float_comparisons_with_nan() {
    let rows: Vec<Vec<ScalarValue>> = [
        (1.0, 2.0),
        (2.0, 1.0),
        (1.0, 1.0),
        (f64::NAN, 1.0),
        (f64::NAN, f64::NAN),
    ]
    .iter()
    .map(|&(a, b)| vec![Float(a), Float(b)])
    .collect();

    for (name, op) in [("lt", "<"), ("gt", ">"), ("eq", "=="), ("ne", "!=")] {
        let src = format!("fn {name}(a: float, b: float) -> bool {{ return a {op} b }}");
        check(&src, name, &rows);
    }
}

#[test]
fn boolean_logic_and_short_circuit() {
    let rows = [
        vec![Bool(false), Bool(false)],
        vec![Bool(false), Bool(true)],
        vec![Bool(true), Bool(false)],
        vec![Bool(true), Bool(true)],
    ];
    check(
        "fn band(a: bool, b: bool) -> bool { return a && b }",
        "band",
        &rows,
    );
    check(
        "fn bor(a: bool, b: bool) -> bool { return a || b }",
        "bor",
        &rows,
    );
    check(
        "fn bnot(a: bool) -> bool { return !a }",
        "bnot",
        &[vec![Bool(true)], vec![Bool(false)]],
    );
}

#[test]
fn control_flow_branches_and_loops() {
    let abs = "fn iabs(n: int) -> int {\n  if n < 0 {\n    return -n\n  }\n  return n\n}";
    check(abs, "iabs", &ints(INT_SAMPLES));

    let clamp = "fn clamp(n: int, lo: int, hi: int) -> int {\n  if n < lo {\n    return lo\n  }\n  if n > hi {\n    return hi\n  }\n  return n\n}";
    check(
        clamp,
        "clamp",
        &[
            vec![Int(5), Int(0), Int(10)],
            vec![Int(-3), Int(0), Int(10)],
            vec![Int(99), Int(0), Int(10)],
        ],
    );

    let sum = "fn sum_to(n: int) -> int {\n  var total = 0\n  var i = 1\n  while i <= n {\n    total = total + i\n    i = i + 1\n  }\n  return total\n}";
    check(sum, "sum_to", &ints(&[0, 1, 5, 10, 100, -5]));
}

#[test]
fn nested_loops_collatz_steps() {
    let src = "fn collatz(start: int) -> int {\n  var n = start\n  var steps = 0\n  while n > 1 {\n    if n % 2 == 0 {\n      n = n / 2\n    } else {\n      n = 3 * n + 1\n    }\n    steps = steps + 1\n  }\n  return steps\n}";
    check(src, "collatz", &ints(&[1, 2, 3, 6, 7, 27]));
}

#[test]
fn unsupported_constructs_are_reported() {
    let cases: &[(&str, &str)] = &[
        ("fn f(s: string) -> int { return 1 }", "f"),
        ("fn f(a: int) -> int { return a ** 2 }", "f"),
        ("fn f(a: float, b: float) -> float { return a % b }", "f"),
        ("fn f(a: int) -> string { return \"hi\" }", "f"),
        ("fn f(a: int) { let x = a }", "f"),
    ];
    for (src, name) in cases {
        let err = analyze_named(src, name)
            .expect_err(&format!("expected `{name}` to be unsupported: {src}"));
        assert!(
            matches!(err, CodegenError::Unsupported(_)),
            "expected Unsupported, got {err:?} for: {src}"
        );
    }
}

#[test]
fn missing_function_is_unsupported() {
    let err = analyze_named("fn a(x: int) -> int { return x }", "b").unwrap_err();
    assert!(matches!(err, CodegenError::Unsupported(_)));
}

#[test]
fn jit_argument_validation_panics() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let scalar = analyze_named("fn f(a: int) -> int { return a }", "f").unwrap();
    let native = jit_compile(&scalar).unwrap();
    let result = catch_unwind(AssertUnwindSafe(|| native.call(&[Int(1), Int(2)])));
    assert!(result.is_err(), "wrong arity should panic");
    let result = catch_unwind(AssertUnwindSafe(|| native.call(&[Float(1.0)])));
    assert!(result.is_err(), "wrong type should panic");
}
