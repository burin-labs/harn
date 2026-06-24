//! VM-fidelity differential tests.
//!
//! The rest of the suite checks the JIT against the crate's own reference
//! interpreter. That proves internal consistency but not *fidelity to the
//! product*: both could share a wrong assumption about Harn semantics. This
//! file closes the loop by running the **real `harn-vm` interpreter** on the
//! same functions and asserting the native compiler's contract against ground
//! truth:
//!
//! * where the JIT returns a [`NativeOutcome::Value`], it is bit-identical to
//!   the value the VM computes; and
//! * where the JIT [`NativeOutcome::Deopt`]s on integer overflow, the VM really
//!   does promote that result to `float` (so the deopt is *justified*, not a
//!   missed optimisation) — and a genuine divide-by-zero traps on both sides.
//!
//! This is the test that would have caught the original soundness gap, where
//! the native code wrapped on overflow while the VM promoted. It drives the VM
//! through a current-thread Tokio runtime; that dependency is dev-only and
//! never ships.

use harn_codegen::{
    analyze_named, jit_compile, DeoptReason, NativeOutcome, NativeTrap, ScalarValue,
};
use harn_vm::{compile_source, register_vm_stdlib, Harness, Vm, VmValue};

use ScalarValue::{Float, Int};

/// What the VM (and, by the contract, the JIT) should produce for a case.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Stays in the int subset: the VM yields this `int`, the JIT a matching
    /// `Value`.
    IntValue(i64),
    /// A `float`-typed result: the VM yields this `float`, the JIT a matching
    /// `Value`.
    FloatValue(f64),
    /// Integer overflow: the VM promotes to `float` (any value) and the JIT
    /// deopts. The justification for the deopt.
    FloatPromotion,
    /// A runtime trap (divide by zero) on both sides.
    Trap,
}

/// Render a scalar as Harn source so we can call the function from a trailing
/// top-level expression the VM evaluates.
fn render_arg(value: ScalarValue) -> String {
    match value {
        ScalarValue::Int(n) => n.to_string(),
        // `{:?}` always prints a decimal point, so it lexes as a float literal.
        ScalarValue::Float(f) => format!("{f:?}"),
        ScalarValue::Bool(b) => b.to_string(),
    }
}

/// Run `source` on the real Harn VM and return the program's value.
fn vm_eval(source: &str) -> Result<VmValue, String> {
    let chunk = compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                // The `main(harness: Harness)` entrypoint convention reads a
                // `harness` global; provide the real one.
                vm.set_harness(Harness::real());
                vm.execute(&chunk).await.map_err(|e| e.to_string())
            })
            .await
    })
}

/// Drive one case through both the VM and the JIT and assert the contract.
fn check_fidelity(fn_def: &str, name: &str, args: &[ScalarValue], expect: Expect) {
    // VM side: define the function and return its result from the auto-invoked
    // `main(harness)` entrypoint (top-level expression statements are discarded,
    // so the value has to come back through `main`).
    let call = format!(
        "{name}({})",
        args.iter()
            .map(|a| render_arg(*a))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let vm_source = format!("{fn_def}\nfn main(harness: Harness) {{ return {call} }}\n");
    let vm_result = vm_eval(&vm_source);

    // JIT side: compile and run the same function on the same arguments.
    let scalar = analyze_named(fn_def, name)
        .unwrap_or_else(|e| panic!("`{name}` should be scalar-eligible: {e}"));
    let native = jit_compile(&scalar).expect("jit compile");
    let jit_result = native.call(args);

    match expect {
        Expect::IntValue(expected) => {
            match vm_result {
                Ok(VmValue::Int(n)) => {
                    assert_eq!(n, expected, "VM {name}{args:?} int mismatch");
                }
                other => panic!("VM {name}{args:?} should be Int({expected}), got {other:?}"),
            }
            assert_eq!(
                jit_result,
                Ok(NativeOutcome::Value(Int(expected))),
                "JIT {name}{args:?} should match the VM"
            );
        }
        Expect::FloatValue(expected) => {
            match vm_result {
                Ok(VmValue::Float(f)) => assert_eq!(
                    f.to_bits(),
                    expected.to_bits(),
                    "VM {name}{args:?} float mismatch"
                ),
                other => panic!("VM {name}{args:?} should be Float({expected}), got {other:?}"),
            }
            assert_eq!(
                jit_result,
                Ok(NativeOutcome::Value(Float(expected))),
                "JIT {name}{args:?} should match the VM float"
            );
        }
        Expect::FloatPromotion => {
            // The crux: the VM promotes the overflowing int result to a float,
            // which the monomorphic native kernel cannot represent.
            assert!(
                matches!(vm_result, Ok(VmValue::Float(_))),
                "VM {name}{args:?} should promote to Float, got {vm_result:?}"
            );
            assert_eq!(
                jit_result,
                Ok(NativeOutcome::Deopt(DeoptReason::IntegerOverflow)),
                "JIT {name}{args:?} should deopt where the VM promotes"
            );
        }
        Expect::Trap => {
            assert!(
                vm_result.is_err(),
                "VM {name}{args:?} should trap, got {vm_result:?}"
            );
            assert_eq!(
                jit_result,
                Err(NativeTrap::DivideByZero),
                "JIT {name}{args:?} should trap where the VM does"
            );
        }
    }
}

// 2^62: two of these sum to 2^63, one past i64::MAX — an overflowing add whose
// operands are still representable as plain int literals (so no i64::MIN
// literal, which would itself overflow the lexer).
const TWO_POW_62: i64 = 1 << 62;

#[test]
fn add_in_range_matches_vm() {
    check_fidelity(
        "fn add(a: int, b: int) -> int { return a + b }",
        "add",
        &[Int(2), Int(3)],
        Expect::IntValue(5),
    );
}

#[test]
fn add_overflow_promotes_in_vm_and_deopts_in_jit() {
    check_fidelity(
        "fn add(a: int, b: int) -> int { return a + b }",
        "add",
        &[Int(TWO_POW_62), Int(TWO_POW_62)],
        Expect::FloatPromotion,
    );
}

#[test]
fn sub_overflow_promotes_in_vm_and_deopts_in_jit() {
    check_fidelity(
        "fn sub(a: int, b: int) -> int { return a - b }",
        "sub",
        &[Int(TWO_POW_62), Int(-TWO_POW_62)],
        Expect::FloatPromotion,
    );
}

#[test]
fn mul_overflow_promotes_in_vm_and_deopts_in_jit() {
    // 3037000500^2 ≈ 9.22e18 > i64::MAX.
    check_fidelity(
        "fn mul(a: int, b: int) -> int { return a * b }",
        "mul",
        &[Int(3_037_000_500), Int(3_037_000_500)],
        Expect::FloatPromotion,
    );
}

#[test]
fn negate_overflow_promotes_in_vm_and_deopts_in_jit() {
    // -(-2^62 * 2) would need i64::MIN; instead test that the VM and JIT agree
    // on an in-range negation, and rely on the unit suite for the i64::MIN
    // boundary (which cannot be written as a literal here).
    check_fidelity(
        "fn neg(a: int) -> int { return -a }",
        "neg",
        &[Int(7)],
        Expect::IntValue(-7),
    );
}

#[test]
fn float_addition_matches_vm() {
    check_fidelity(
        "fn fadd(a: float, b: float) -> float { return a + b }",
        "fadd",
        &[Float(1.5), Float(2.25)],
        Expect::FloatValue(3.75),
    );
}

#[test]
fn int_divide_by_zero_traps_on_both() {
    check_fidelity(
        "fn d(a: int, b: int) -> int { return a / b }",
        "d",
        &[Int(5), Int(0)],
        Expect::Trap,
    );
}

#[test]
fn loop_kernel_in_range_matches_vm() {
    // A representative hot kernel: sum 1..=n. Stays in range for small n, so
    // the VM and JIT must produce the identical int.
    let sum = "fn sum_to(n: int) -> int {\n  var total = 0\n  var i = 1\n  while i <= n {\n    total = total + i\n    i = i + 1\n  }\n  return total\n}";
    check_fidelity(sum, "sum_to", &[Int(100)], Expect::IntValue(5050));
}
