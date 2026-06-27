//! A small reference interpreter for [`ScalarFunction`].
//!
//! It serves three purposes: it is the executable specification the JIT must
//! match (its arithmetic mirrors the Harn VM — integer ops that *deopt* to the
//! VM on `i64` overflow rather than wrapping, IEEE-754 floats), it is the
//! differential-test oracle, and it is a pure-Rust fallback for callers that
//! want scalar evaluation without linking a code generator.
//!
//! Like the JIT (and the VM), an overflowing integer `+`/`-`/`*`/negation is
//! reported as [`NativeOutcome::Deopt`] — see [`crate::outcome`] — not silently
//! wrapped. `i64::MIN / -1` overflows the same way, so it deopts too; integer
//! `%` keeps its wrapping semantics (`i64::MIN % -1 == 0`). All integer `/`/`%`
//! still trap only on a zero divisor.

use crate::bytecode::{BinOp, CmpOp, Instr};
use crate::error::NativeTrap;
use crate::outcome::{DeoptReason, NativeOutcome};
use crate::value::ScalarValue;
use crate::verify::{ScalarFunction, Terminator};

/// Upper bound on executed instructions, guarding the reference interpreter
/// against runaway loops in malformed input. Generous enough for any realistic
/// scalar kernel.
const STEP_BUDGET: u64 = 1 << 32;

/// An error from reference evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A runtime trap that the JIT would also raise (e.g. integer `x / 0`).
    Trap(NativeTrap),
    /// An argument count or type mismatch against the function signature.
    Signature(String),
    /// The step budget was exhausted (likely a non-terminating loop).
    Budget,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trap(trap) => write!(f, "{trap}"),
            Self::Signature(msg) => write!(f, "signature mismatch: {msg}"),
            Self::Budget => f.write_str("evaluation step budget exhausted"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Evaluate `func` with `args`, returning the scalar [`NativeOutcome`].
///
/// The result is a [`NativeOutcome::Value`] bit-identical to the Harn VM, or a
/// [`NativeOutcome::Deopt`] when an integer operation overflowed and the VM
/// would promote to `float`.
///
/// # Errors
///
/// Returns [`EvalError`] on a signature mismatch, a runtime trap (integer
/// divide by zero), or budget exhaustion.
pub fn evaluate(func: &ScalarFunction, args: &[ScalarValue]) -> Result<NativeOutcome, EvalError> {
    if args.len() != func.params.len() {
        return Err(EvalError::Signature(format!(
            "expected {} argument(s), got {}",
            func.params.len(),
            args.len()
        )));
    }
    for (idx, (arg, expected)) in args.iter().zip(&func.params).enumerate() {
        if arg.ty() != *expected {
            return Err(EvalError::Signature(format!(
                "argument {idx} expected {expected}, got {}",
                arg.ty()
            )));
        }
    }

    let mut locals: Vec<Option<ScalarValue>> = vec![None; func.slot_count()];
    for (slot, arg) in args.iter().enumerate() {
        locals[slot] = Some(*arg);
    }

    let mut block_idx = 0usize;
    let mut stack: Vec<ScalarValue> = Vec::new();
    let mut steps = 0u64;

    loop {
        let block = &func.blocks[block_idx];
        for instr in &block.body {
            steps += 1;
            if steps > STEP_BUDGET {
                return Err(EvalError::Budget);
            }
            if let Some(reason) = step(&mut stack, &mut locals, instr)? {
                return Ok(NativeOutcome::Deopt(reason));
            }
        }
        match block.term {
            Terminator::Return => {
                return stack
                    .pop()
                    .map(NativeOutcome::Value)
                    .ok_or_else(|| EvalError::Signature("return with empty stack".into()));
            }
            Terminator::Jump(target) => block_idx = target,
            Terminator::Branch { on_true, on_false } => {
                let cond = matches!(stack.last(), Some(ScalarValue::Bool(true)));
                block_idx = if cond { on_true } else { on_false };
            }
        }
    }
}

/// Apply one instruction. Returns `Ok(Some(reason))` when the instruction
/// deoptimised (integer overflow the VM would promote to `float`); the caller
/// stops and yields [`NativeOutcome::Deopt`].
fn step(
    stack: &mut Vec<ScalarValue>,
    locals: &mut [Option<ScalarValue>],
    instr: &Instr,
) -> Result<Option<DeoptReason>, EvalError> {
    match instr {
        Instr::Const(value) => stack.push(*value),
        Instr::Bin(op) => {
            let b = pop(stack);
            let a = pop(stack);
            match eval_bin(*op, a, b)? {
                Some(v) => stack.push(v),
                None => return Ok(Some(DeoptReason::IntegerOverflow)),
            }
        }
        Instr::Cmp(op) => {
            let b = pop(stack);
            let a = pop(stack);
            stack.push(ScalarValue::Bool(eval_cmp(*op, a, b)));
        }
        Instr::Neg => {
            let a = pop(stack);
            let value = match a {
                // `-i64::MIN` overflows; the VM promotes it to float, so deopt.
                ScalarValue::Int(n) => match n.checked_neg() {
                    Some(neg) => ScalarValue::Int(neg),
                    None => return Ok(Some(DeoptReason::IntegerOverflow)),
                },
                ScalarValue::Float(x) => ScalarValue::Float(-x),
                ScalarValue::Bool(_) => unreachable!("verified"),
            };
            stack.push(value);
        }
        Instr::Not => {
            let a = pop(stack);
            stack.push(ScalarValue::Bool(!matches!(a, ScalarValue::Bool(true))));
        }
        Instr::Pop => {
            pop(stack);
        }
        Instr::Dup => {
            let top = *stack.last().expect("verified non-empty");
            stack.push(top);
        }
        Instr::Swap => {
            let len = stack.len();
            stack.swap(len - 1, len - 2);
        }
        Instr::GetLocal(slot) => {
            let value = locals[*slot as usize].expect("verified initialised");
            stack.push(value);
        }
        Instr::DefLocal(slot) | Instr::SetLocal(slot) => {
            let value = pop(stack);
            locals[*slot as usize] = Some(value);
        }
        Instr::Nop => {}
        // The verifier proved this unit is only ever discarded; a placeholder
        // keeps the stack balanced and is never observed.
        Instr::PushUnit => stack.push(ScalarValue::Int(0)),
        Instr::Jump(_) | Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) | Instr::Return => {
            unreachable!("terminators are not in block bodies")
        }
    }
    Ok(None)
}

fn pop(stack: &mut Vec<ScalarValue>) -> ScalarValue {
    stack.pop().expect("verified non-empty operand stack")
}

/// Evaluate a binary op. Returns `Ok(None)` when an integer `+`/`-`/`*` or
/// `i64::MIN / -1` overflowed `i64` — the VM promotes to `float`, so the caller
/// deopts — matching the JIT's overflow guards. Integer `%` wraps (as the VM
/// does, `i64::MIN % -1 == 0`); both `/` and `%` trap only on a zero divisor.
fn eval_bin(op: BinOp, a: ScalarValue, b: ScalarValue) -> Result<Option<ScalarValue>, EvalError> {
    match (a, b) {
        (ScalarValue::Int(x), ScalarValue::Int(y)) => Ok(match op {
            // checked_* mirror the VM's promote-on-overflow: None -> deopt.
            BinOp::Add => x.checked_add(y).map(ScalarValue::Int),
            BinOp::Sub => x.checked_sub(y).map(ScalarValue::Int),
            BinOp::Mul => x.checked_mul(y).map(ScalarValue::Int),
            BinOp::Div => {
                if y == 0 {
                    return Err(EvalError::Trap(NativeTrap::DivideByZero));
                }
                // `i64::MIN / -1` overflows; the VM promotes to float, so deopt
                // (`None`) like `+`/`-`/`*`. `checked_div` is `None` only there.
                x.checked_div(y).map(ScalarValue::Int)
            }
            BinOp::Mod => {
                if y == 0 {
                    return Err(EvalError::Trap(NativeTrap::DivideByZero));
                }
                Some(ScalarValue::Int(x.wrapping_rem(y)))
            }
        }),
        (ScalarValue::Float(x), ScalarValue::Float(y)) => Ok(Some(ScalarValue::Float(match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => x / y,
            BinOp::Mod => unreachable!("float modulo is rejected by the verifier"),
        }))),
        _ => unreachable!("verified matching numeric operands"),
    }
}

fn eval_cmp(op: CmpOp, a: ScalarValue, b: ScalarValue) -> bool {
    match (a, b) {
        (ScalarValue::Int(x), ScalarValue::Int(y)) => cmp_ord(op, x, y),
        (ScalarValue::Float(x), ScalarValue::Float(y)) => match op {
            // IEEE-754: a NaN operand makes `==`/ordered comparisons false and
            // `!=` true. Rust's float operators already do exactly this.
            CmpOp::Eq => x == y,
            CmpOp::Ne => x != y,
            CmpOp::Lt => x < y,
            CmpOp::Gt => x > y,
            CmpOp::Le => x <= y,
            CmpOp::Ge => x >= y,
        },
        (ScalarValue::Bool(x), ScalarValue::Bool(y)) => match op {
            CmpOp::Eq => x == y,
            CmpOp::Ne => x != y,
            _ => unreachable!("verified equality-only comparison on bool"),
        },
        _ => unreachable!("verified matching operands"),
    }
}

fn cmp_ord<T: PartialOrd + PartialEq>(op: CmpOp, x: T, y: T) -> bool {
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::Lt => x < y,
        CmpOp::Gt => x > y,
        CmpOp::Le => x <= y,
        CmpOp::Ge => x >= y,
    }
}
