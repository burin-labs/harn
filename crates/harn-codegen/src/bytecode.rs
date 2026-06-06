//! Decoding of the Harn VM bytecode subset that the native compiler
//! understands.
//!
//! We deliberately avoid coupling to `harn-vm`'s private dispatch tables: the
//! only thing this module relies on is that [`harn_vm::Op`] is a public
//! `#[repr(u8)]` enum, so `Op::Foo as u8` yields its canonical byte. That
//! keeps the whole compiler additive — nothing in the shipped VM changes, and
//! the distributed binary never links Cranelift.
//!
//! Decoding is *reachability driven* (see [`crate::verify`]): the Harn
//! compiler routinely emits an unreachable `Nil; Return` epilogue after an
//! explicit `return`, and `Nil` is outside the scalar subset. Walking control
//! flow instead of the raw byte stream means that dead epilogue never
//! disqualifies an otherwise-scalar function.

use harn_vm::{Constant, Op};

use crate::error::CodegenError;
use crate::value::ScalarValue;

/// A scalar binary arithmetic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// A scalar comparison operator. Always produces a `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// A decoded instruction in the scalar subset. Operands are fully resolved
/// (constants materialised, jump targets as absolute byte offsets) so neither
/// the verifier nor the backends need to re-touch the raw byte stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Instr {
    /// Push a scalar constant.
    Const(ScalarValue),
    /// Pop two operands, push `a <op> b`.
    Bin(BinOp),
    /// Pop two operands, push the boolean comparison result.
    Cmp(CmpOp),
    /// Pop one operand, push its arithmetic negation.
    Neg,
    /// Pop one boolean, push its logical negation.
    Not,
    /// Discard the top operand.
    Pop,
    /// Duplicate the top operand.
    Dup,
    /// Swap the top two operands.
    Swap,
    /// Push the value of a local slot.
    GetLocal(u16),
    /// Pop a value and bind it to a local slot (first definition).
    DefLocal(u16),
    /// Pop a value and assign it to an already-defined local slot.
    SetLocal(u16),
    /// Unconditional jump to an absolute byte offset.
    Jump(usize),
    /// Branch to an absolute byte offset when the (peeked, not popped) top of
    /// stack is falsy; fall through otherwise.
    JumpIfFalse(usize),
    /// Branch to an absolute byte offset when the (peeked) top of stack is
    /// truthy; fall through otherwise.
    JumpIfTrue(usize),
    /// Pop the result and return it from the function.
    Return,
    /// No operand-stack or slot effect. Used for bytecode that only touches
    /// runtime bookkeeping irrelevant to scalar code — `PushScope`/`PopScope`
    /// manage the name environment and lexical scope depth, neither of which a
    /// slot-based scalar function observes.
    Nop,
    /// Push the `nil` unit value. Harn emits this for the discarded result of
    /// statement-level `while`/`if`/blocks, always immediately followed by a
    /// `Pop`. The verifier strips those `PushUnit; Pop` pairs; any surviving
    /// unit (e.g. an implicit nil return) falls outside the scalar subset.
    PushUnit,
}

impl Instr {
    /// True for instructions that end a basic block.
    pub(crate) const fn is_terminator(&self) -> bool {
        matches!(
            self,
            Self::Jump(_) | Self::JumpIfFalse(_) | Self::JumpIfTrue(_) | Self::Return
        )
    }
}

/// A single decoded instruction together with its position in the byte stream
/// and the offset of the following instruction (its fall-through successor).
#[derive(Debug, Clone)]
pub(crate) struct Decoded {
    pub next: usize,
    pub instr: Instr,
}

/// Decode the instruction at `ip`, resolving constant-pool references against
/// `constants`. Returns the decoded instruction and the offset of the next
/// instruction, or [`CodegenError::Unsupported`] for any opcode outside the
/// scalar subset.
pub(crate) fn decode_at(
    code: &[u8],
    constants: &[Constant],
    ip: usize,
) -> Result<Decoded, CodegenError> {
    let byte = *code
        .get(ip)
        .ok_or_else(|| CodegenError::verify(format!("instruction pointer {ip} out of bounds")))?;

    let read_u16 = |pos: usize| -> Result<u16, CodegenError> {
        match (code.get(pos), code.get(pos + 1)) {
            (Some(hi), Some(lo)) => Ok((u16::from(*hi) << 8) | u16::from(*lo)),
            _ => Err(CodegenError::verify("truncated u16 operand")),
        }
    };

    // Helper that builds a `Decoded` for a bare (operand-less) instruction.
    let bare = |instr: Instr| {
        Ok(Decoded {
            next: ip + 1,
            instr,
        })
    };
    // Helper for instructions with a single u16 operand.
    let with_u16 = |instr: Instr| {
        Ok(Decoded {
            next: ip + 3,
            instr,
        })
    };

    // === bare scalar ops ===
    if byte == Op::True as u8 {
        return bare(Instr::Const(ScalarValue::Bool(true)));
    }
    if byte == Op::False as u8 {
        return bare(Instr::Const(ScalarValue::Bool(false)));
    }
    if let Some(op) = bare_binop(byte) {
        return bare(Instr::Bin(op));
    }
    if let Some(op) = bare_cmpop(byte) {
        return bare(Instr::Cmp(op));
    }
    if byte == Op::Negate as u8 {
        return bare(Instr::Neg);
    }
    if byte == Op::Not as u8 {
        return bare(Instr::Not);
    }
    if byte == Op::Pop as u8 {
        return bare(Instr::Pop);
    }
    if byte == Op::Dup as u8 {
        return bare(Instr::Dup);
    }
    if byte == Op::Swap as u8 {
        return bare(Instr::Swap);
    }
    if byte == Op::Return as u8 {
        return bare(Instr::Return);
    }
    // Lexical-scope bookkeeping with no effect on the operand stack or
    // slot-indexed locals; transparent to scalar code.
    if byte == Op::PushScope as u8 || byte == Op::PopScope as u8 {
        return bare(Instr::Nop);
    }
    if byte == Op::Nil as u8 {
        return bare(Instr::PushUnit);
    }

    // === operand-carrying ops ===
    if byte == Op::Constant as u8 {
        let idx = read_u16(ip + 1)? as usize;
        let constant = constants
            .get(idx)
            .ok_or_else(|| CodegenError::verify(format!("constant index {idx} out of bounds")))?;
        return with_u16(Instr::Const(scalar_constant(constant)?));
    }
    if byte == Op::GetLocalSlot as u8 {
        return with_u16(Instr::GetLocal(read_u16(ip + 1)?));
    }
    if byte == Op::DefLocalSlot as u8 {
        return with_u16(Instr::DefLocal(read_u16(ip + 1)?));
    }
    if byte == Op::SetLocalSlot as u8 {
        return with_u16(Instr::SetLocal(read_u16(ip + 1)?));
    }
    if byte == Op::Jump as u8 {
        return with_u16(Instr::Jump(read_u16(ip + 1)? as usize));
    }
    if byte == Op::JumpIfFalse as u8 {
        return with_u16(Instr::JumpIfFalse(read_u16(ip + 1)? as usize));
    }
    if byte == Op::JumpIfTrue as u8 {
        return with_u16(Instr::JumpIfTrue(read_u16(ip + 1)? as usize));
    }

    Err(CodegenError::unsupported(unsupported_label(byte)))
}

/// Map the operand-less arithmetic opcodes (generic and int/float-typed) onto
/// a [`BinOp`]. `Pow` is intentionally absent: its result type depends on the
/// *value* of the exponent at runtime, so it cannot be statically monomorphic.
fn bare_binop(byte: u8) -> Option<BinOp> {
    let table = [
        (Op::Add, BinOp::Add),
        (Op::Sub, BinOp::Sub),
        (Op::Mul, BinOp::Mul),
        (Op::Div, BinOp::Div),
        (Op::Mod, BinOp::Mod),
        (Op::AddInt, BinOp::Add),
        (Op::SubInt, BinOp::Sub),
        (Op::MulInt, BinOp::Mul),
        (Op::DivInt, BinOp::Div),
        (Op::ModInt, BinOp::Mod),
        (Op::AddFloat, BinOp::Add),
        (Op::SubFloat, BinOp::Sub),
        (Op::MulFloat, BinOp::Mul),
        (Op::DivFloat, BinOp::Div),
        (Op::ModFloat, BinOp::Mod),
    ];
    table
        .into_iter()
        .find(|(op, _)| byte == *op as u8)
        .map(|(_, bin)| bin)
}

/// Map the operand-less comparison opcodes (generic and typed) onto a
/// [`CmpOp`].
fn bare_cmpop(byte: u8) -> Option<CmpOp> {
    let table = [
        (Op::Equal, CmpOp::Eq),
        (Op::NotEqual, CmpOp::Ne),
        (Op::Less, CmpOp::Lt),
        (Op::Greater, CmpOp::Gt),
        (Op::LessEqual, CmpOp::Le),
        (Op::GreaterEqual, CmpOp::Ge),
        (Op::EqualInt, CmpOp::Eq),
        (Op::NotEqualInt, CmpOp::Ne),
        (Op::LessInt, CmpOp::Lt),
        (Op::GreaterInt, CmpOp::Gt),
        (Op::LessEqualInt, CmpOp::Le),
        (Op::GreaterEqualInt, CmpOp::Ge),
        (Op::EqualFloat, CmpOp::Eq),
        (Op::NotEqualFloat, CmpOp::Ne),
        (Op::LessFloat, CmpOp::Lt),
        (Op::GreaterFloat, CmpOp::Gt),
        (Op::LessEqualFloat, CmpOp::Le),
        (Op::GreaterEqualFloat, CmpOp::Ge),
        (Op::EqualBool, CmpOp::Eq),
        (Op::NotEqualBool, CmpOp::Ne),
    ];
    table
        .into_iter()
        .find(|(op, _)| byte == *op as u8)
        .map(|(_, cmp)| cmp)
}

/// Resolve a constant-pool entry to a scalar value, or reject it.
fn scalar_constant(constant: &Constant) -> Result<ScalarValue, CodegenError> {
    match constant {
        Constant::Int(n) => Ok(ScalarValue::Int(*n)),
        Constant::Float(f) => Ok(ScalarValue::Float(*f)),
        Constant::Bool(b) => Ok(ScalarValue::Bool(*b)),
        Constant::String(_) => Err(CodegenError::unsupported("string constant")),
        Constant::Nil => Err(CodegenError::unsupported("nil constant")),
        Constant::Duration(_) => Err(CodegenError::unsupported("duration constant")),
    }
}

/// Best-effort human-readable label for an unsupported opcode, for diagnostics.
fn unsupported_label(byte: u8) -> String {
    if byte == Op::Pow as u8 {
        return "exponentiation (`**`) has a value-dependent result type".to_string();
    }
    if byte == Op::Concat as u8 {
        return "string concatenation".to_string();
    }
    if byte == Op::Call as u8 || byte == Op::CallBuiltin as u8 || byte == Op::TailCall as u8 {
        return "function call".to_string();
    }
    if byte == Op::GetVar as u8 || byte == Op::SetVar as u8 {
        return "non-local (captured/global) variable access".to_string();
    }
    format!("opcode byte 0x{byte:02x}")
}
