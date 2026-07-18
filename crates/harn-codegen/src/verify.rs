//! Bytecode verification and scalar type inference.
//!
//! This stage turns a flat slice of [`Instr`]s into a typed control-flow graph
//! ([`ScalarFunction`]) that the backends can lower directly. It does three
//! things:
//!
//! 1. **Reachability decode** — walk control flow from the entry, decoding only
//!    reachable instructions. Dead epilogues never disqualify a function.
//! 2. **Basic-block construction** — split at branch targets and the
//!    fall-through after a conditional branch.
//! 3. **Type inference** — a monotone fixpoint that assigns one static
//!    [`ScalarType`] to every operand-stack position (per block entry) and
//!    every local slot, rejecting anything that is not provably monomorphic.
//!
//! The inference is deliberately conservative: a slot that is `int` on one
//! path and `float` on another, or a control-flow merge with mismatched stack
//! shapes, is rejected as [`CodegenError::Verify`]. That is what lets the
//! backend use unboxed machine registers with zero runtime tag checks.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use harn_vm::Constant;

use crate::bytecode::{decode_at, BinOp, CmpOp, Decoded, Instr};
use crate::error::CodegenError;
use crate::value::ScalarType;

/// A verified, fully typed scalar function ready for lowering.
#[derive(Debug, Clone)]
pub struct ScalarFunction {
    /// Diagnostic name (the Harn function name, when known).
    pub name: String,
    /// Parameter types, in declaration order. They occupy local slots
    /// `0..params.len()`.
    pub params: Vec<ScalarType>,
    /// The function's return type, inferred from every reachable `return`.
    pub ret: ScalarType,
    /// Static type of every local slot, indexed by slot id.
    pub slot_types: Vec<ScalarType>,
    /// Basic blocks. Block `0` is always the entry.
    pub blocks: Vec<Block>,
}

impl ScalarFunction {
    /// Number of local slots (parameters plus `let`/`const` bindings).
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slot_types.len()
    }
}

/// A basic block: a straight-line run of body instructions ending in a single
/// terminator.
#[derive(Debug, Clone)]
pub struct Block {
    /// Operand-stack types on entry, bottom to top. These become the block's
    /// SSA parameters in the lowered IR.
    pub stack_in: Vec<ScalarType>,
    /// Non-terminator instructions, in order.
    pub body: Vec<Instr>,
    /// How the block transfers control.
    pub term: Terminator,
}

/// The control transfer at the end of a [`Block`]. Successor blocks are
/// referenced by index into [`ScalarFunction::blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    /// Return the popped top-of-stack value.
    Return,
    /// Unconditional transfer (covers both explicit `Jump` and implicit
    /// fall-through into a shared block).
    Jump(usize),
    /// Branch on the (peeked, not popped) boolean top-of-stack.
    Branch { on_true: usize, on_false: usize },
}

/// Verify a decoded scalar function body and infer its types.
///
/// `code`/`constants` are the function's bytecode and constant pool; `params`
/// are the statically known parameter types occupying the leading slots.
pub fn verify(
    name: impl Into<String>,
    code: &[u8],
    constants: &[Constant],
    params: &[ScalarType],
) -> Result<ScalarFunction, CodegenError> {
    if code.is_empty() {
        return Err(CodegenError::verify("empty function body"));
    }

    let decoded = decode_reachable(code, constants)?;
    let (blocks_raw, block_of) = build_blocks(&decoded)?;
    infer_types(name.into(), &blocks_raw, &block_of, params)
}

/// Decode every reachable instruction, keyed by byte offset.
fn decode_reachable(
    code: &[u8],
    constants: &[Constant],
) -> Result<BTreeMap<usize, Decoded>, CodegenError> {
    let mut decoded: BTreeMap<usize, Decoded> = BTreeMap::new();
    let mut worklist = vec![0usize];

    while let Some(ip) = worklist.pop() {
        if decoded.contains_key(&ip) {
            continue;
        }
        let d = decode_at(code, constants, ip)?;
        let mut successors = Vec::new();
        match &d.instr {
            Instr::Return => {}
            Instr::Jump(target) => successors.push(*target),
            Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                successors.push(*target);
                successors.push(d.next);
            }
            _ => successors.push(d.next),
        }
        decoded.insert(ip, d);
        worklist.extend(successors);
    }

    Ok(decoded)
}

/// Structural (pre-typing) block representation.
struct RawBlock {
    body: Vec<Instr>,
    term: Terminator,
}

/// Partition the reachable instructions into basic blocks and resolve every
/// terminator to successor block indices.
fn build_blocks(
    decoded: &BTreeMap<usize, Decoded>,
) -> Result<(Vec<RawBlock>, HashMap<usize, usize>), CodegenError> {
    // Leaders: entry, every branch target, and the fall-through after a
    // conditional branch.
    let mut leaders: BTreeSet<usize> = BTreeSet::new();
    leaders.insert(0);
    for d in decoded.values() {
        match &d.instr {
            Instr::Jump(target) => {
                leaders.insert(*target);
            }
            Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                leaders.insert(*target);
                leaders.insert(d.next);
            }
            _ => {}
        }
    }

    let block_of: HashMap<usize, usize> = leaders
        .iter()
        .enumerate()
        .map(|(idx, offset)| (*offset, idx))
        .collect();

    let resolve = |offset: usize| -> Result<usize, CodegenError> {
        block_of
            .get(&offset)
            .copied()
            .ok_or_else(|| CodegenError::verify(format!("jump to non-leader offset {offset}")))
    };

    let mut blocks = Vec::with_capacity(leaders.len());
    for &leader in &leaders {
        let mut body = Vec::new();
        let mut ip = leader;
        let term = loop {
            let d = decoded
                .get(&ip)
                .ok_or_else(|| CodegenError::verify(format!("unreachable leader at {ip}")))?;
            if d.instr.is_terminator() {
                break match &d.instr {
                    Instr::Return => Terminator::Return,
                    Instr::Jump(target) => Terminator::Jump(resolve(*target)?),
                    Instr::JumpIfFalse(target) => Terminator::Branch {
                        on_false: resolve(*target)?,
                        on_true: resolve(d.next)?,
                    },
                    Instr::JumpIfTrue(target) => Terminator::Branch {
                        on_true: resolve(*target)?,
                        on_false: resolve(d.next)?,
                    },
                    _ => unreachable!("is_terminator covered above"),
                };
            }
            body.push(d.instr.clone());
            // Stop before the next leader: that boundary becomes an implicit
            // fall-through edge.
            if leaders.contains(&d.next) {
                break Terminator::Jump(resolve(d.next)?);
            }
            if !decoded.contains_key(&d.next) {
                return Err(CodegenError::unsupported(
                    "control falls off the end of the function (implicit nil return)",
                ));
            }
            ip = d.next;
        };
        blocks.push(RawBlock {
            body: strip_unit_discards(body),
            term,
        });
    }

    Ok((blocks, block_of))
}

/// Remove `PushUnit; Pop` peepholes — the discarded `nil` result of a
/// statement-level `while`/`if`/block. Any `PushUnit` that survives (because
/// it is not immediately discarded) is left in place and rejected later.
fn strip_unit_discards(body: Vec<Instr>) -> Vec<Instr> {
    let mut out: Vec<Instr> = Vec::with_capacity(body.len());
    let mut iter = body.into_iter().peekable();
    while let Some(instr) = iter.next() {
        if matches!(instr, Instr::PushUnit) && matches!(iter.peek(), Some(Instr::Pop)) {
            iter.next();
            continue;
        }
        out.push(instr);
    }
    out
}

/// An abstract operand-stack entry during inference.
///
/// `Unit` tracks the `nil` produced by statement-level expressions (`while`,
/// `if`, blocks). It is allowed to flow along the stack and across control-flow
/// merges but may only ever be discarded by a `Pop` — never used in an
/// operation, stored, branched on, or returned. In the lowered IR a surviving
/// unit is encoded as a throwaway `int` placeholder (see [`StackTy::to_scalar`]),
/// which the verifier has proven is never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackTy {
    Scalar(ScalarType),
    Unit,
}

impl StackTy {
    /// The scalar type, or an error naming the offending unit use.
    fn require_scalar(self, context: &str) -> Result<ScalarType, CodegenError> {
        match self {
            Self::Scalar(ty) => Ok(ty),
            Self::Unit => Err(CodegenError::unsupported(format!(
                "nil value used in {context} (only discarded statement results are allowed)"
            ))),
        }
    }

    /// Public-IR encoding: units become a harmless `int` placeholder that the
    /// verifier has proven is only ever discarded.
    const fn to_scalar(self) -> ScalarType {
        match self {
            Self::Scalar(ty) => ty,
            Self::Unit => ScalarType::Int,
        }
    }
}

/// Abstract operand stack during inference/simulation.
type TypeStack = Vec<StackTy>;

/// Run the monotone fixpoint that assigns types to stack positions and slots,
/// then assemble the final [`ScalarFunction`].
fn infer_types(
    name: String,
    blocks: &[RawBlock],
    block_of: &HashMap<usize, usize>,
    params: &[ScalarType],
) -> Result<ScalarFunction, CodegenError> {
    let entry = *block_of
        .get(&0)
        .ok_or_else(|| CodegenError::verify("missing entry block"))?;

    let slot_count = slot_count(blocks, params.len());
    let mut slot_types: Vec<Option<ScalarType>> = vec![None; slot_count];
    for (slot, ty) in params.iter().enumerate() {
        slot_types[slot] = Some(*ty);
    }

    let mut block_in: Vec<Option<TypeStack>> = vec![None; blocks.len()];
    block_in[entry] = Some(Vec::new());

    // Monotone fixpoint: each pass can only learn more slot/stack types. The
    // lattice is finite (slots × types, blocks × bounded stack shapes), so it
    // terminates. `Pending` (a read of a not-yet-typed slot) is tolerated here
    // and only becomes an error in the final validation pass.
    loop {
        let before = known_count(&slot_types) + block_in.iter().filter(|s| s.is_some()).count();
        for b in 0..blocks.len() {
            let Some(stack_in) = block_in[b].clone() else {
                continue;
            };
            simulate_block(
                &blocks[b],
                stack_in,
                &mut slot_types,
                &mut block_in,
                &mut None,
                false,
            )?;
        }
        let after = known_count(&slot_types) + block_in.iter().filter(|s| s.is_some()).count();
        if after == before {
            break;
        }
    }

    // Final authoritative pass: pending slots are now hard errors, and we
    // gather the (consistent) return type.
    let mut ret: Option<ScalarType> = None;
    for b in 0..blocks.len() {
        let Some(stack_in) = block_in[b].clone() else {
            continue; // genuinely unreachable block (no predecessor typed it)
        };
        simulate_block(
            &blocks[b],
            stack_in,
            &mut slot_types,
            &mut block_in,
            &mut ret,
            true,
        )?;
    }

    let ret = ret.ok_or_else(|| {
        CodegenError::unsupported("function never returns a scalar value (implicit nil return)")
    })?;

    let slot_types = slot_types
        .into_iter()
        .map(|ty| ty.unwrap_or(ScalarType::Int))
        .collect();

    let blocks = blocks
        .iter()
        .enumerate()
        .map(|(idx, raw)| Block {
            stack_in: block_in[idx]
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(StackTy::to_scalar)
                .collect(),
            body: raw.body.clone(),
            term: raw.term,
        })
        .collect();

    Ok(ScalarFunction {
        name,
        params: params.to_vec(),
        ret,
        slot_types,
        blocks,
    })
}

/// The maximum slot index referenced anywhere, plus one — or the parameter
/// count, whichever is larger.
fn slot_count(blocks: &[RawBlock], param_count: usize) -> usize {
    let mut max = param_count;
    for block in blocks {
        for instr in &block.body {
            let slot = match instr {
                Instr::GetLocal(s) | Instr::DefLocal(s) | Instr::SetLocal(s) => Some(*s as usize),
                _ => None,
            };
            if let Some(slot) = slot {
                max = max.max(slot + 1);
            }
        }
    }
    max
}

fn known_count(slots: &[Option<ScalarType>]) -> usize {
    slots.iter().filter(|s| s.is_some()).count()
}

/// Simulate one block's type effects.
///
/// Updates `slot_types` and propagates the exit stack to successor `block_in`
/// entries. When `pending_is_error` is false, a read of an untyped slot simply
/// stops the block early (it will be retried once the slot is typed). When
/// true, it is reported as an error.
fn simulate_block(
    block: &RawBlock,
    stack_in: TypeStack,
    slot_types: &mut [Option<ScalarType>],
    block_in: &mut [Option<TypeStack>],
    ret: &mut Option<ScalarType>,
    pending_is_error: bool,
) -> Result<(), CodegenError> {
    let mut stack = stack_in;

    for instr in &block.body {
        if step_type(&mut stack, slot_types, instr, pending_is_error)? {
            // Pending read of an untyped slot: stop here for now.
            return Ok(());
        }
    }

    match block.term {
        Terminator::Return => {
            let entry = stack
                .pop()
                .ok_or_else(|| CodegenError::verify("return with empty operand stack"))?;
            let ty = entry.require_scalar("a return value")?;
            match ret {
                Some(existing) if *existing != ty => {
                    return Err(CodegenError::verify(format!(
                        "inconsistent return types: {existing} vs {ty}"
                    )));
                }
                slot => *slot = Some(ty),
            }
        }
        Terminator::Jump(target) => propagate(block_in, target, &stack)?,
        Terminator::Branch { on_true, on_false } => {
            match stack.last() {
                Some(StackTy::Scalar(ScalarType::Bool)) => {}
                Some(StackTy::Scalar(other)) => {
                    return Err(CodegenError::verify(format!(
                        "branch condition must be bool, found {other}"
                    )));
                }
                Some(StackTy::Unit) => {
                    return Err(CodegenError::verify(
                        "branch condition must be bool, found nil",
                    ));
                }
                None => return Err(CodegenError::verify("branch with empty operand stack")),
            }
            propagate(block_in, on_true, &stack)?;
            propagate(block_in, on_false, &stack)?;
        }
    }

    Ok(())
}

/// Propagate an exit stack into a successor's entry, requiring agreement with
/// any previously recorded shape.
fn propagate(
    block_in: &mut [Option<TypeStack>],
    target: usize,
    stack: &TypeStack,
) -> Result<(), CodegenError> {
    match &block_in[target] {
        Some(existing) if existing != stack => Err(CodegenError::verify(format!(
            "operand-stack shape mismatch at control-flow merge into block {target}: \
             {existing:?} vs {stack:?}"
        ))),
        Some(_) => Ok(()),
        None => {
            block_in[target] = Some(stack.clone());
            Ok(())
        }
    }
}

/// Apply one instruction's type transition to the abstract stack. Returns
/// `Ok(true)` when the instruction is a *pending* read of an untyped slot and
/// the caller should stop simulating this block for now.
fn step_type(
    stack: &mut TypeStack,
    slot_types: &mut [Option<ScalarType>],
    instr: &Instr,
    pending_is_error: bool,
) -> Result<bool, CodegenError> {
    match instr {
        Instr::Const(value) => stack.push(StackTy::Scalar(value.ty())),
        Instr::PushUnit => stack.push(StackTy::Unit),
        Instr::Bin(op) => {
            let (a, b) = pop2_scalar(stack, "an arithmetic operand")?;
            if a != b {
                return Err(CodegenError::verify(format!(
                    "binary operands must match: {a} vs {b}"
                )));
            }
            match (op, a) {
                (_, ScalarType::Bool) => {
                    return Err(CodegenError::verify("arithmetic on bool operands"));
                }
                (BinOp::Mod, ScalarType::Float) => {
                    return Err(CodegenError::unsupported(
                        "float modulo (no native frem; would need a libm call)",
                    ));
                }
                _ => {}
            }
            stack.push(StackTy::Scalar(a));
        }
        Instr::Cmp(op) => {
            let (a, b) = pop2_scalar(stack, "a comparison operand")?;
            if a != b {
                return Err(CodegenError::verify(format!(
                    "comparison operands must match: {a} vs {b}"
                )));
            }
            if matches!(a, ScalarType::Bool) && !matches!(op, CmpOp::Eq | CmpOp::Ne) {
                return Err(CodegenError::verify("ordered comparison on bool operands"));
            }
            stack.push(StackTy::Scalar(ScalarType::Bool));
        }
        Instr::Neg => {
            let a = pop1(stack)?.require_scalar("negation")?;
            if matches!(a, ScalarType::Bool) {
                return Err(CodegenError::verify("negation of bool operand"));
            }
            stack.push(StackTy::Scalar(a));
        }
        Instr::Not => {
            let a = pop1(stack)?.require_scalar("logical not")?;
            if !matches!(a, ScalarType::Bool) {
                return Err(CodegenError::verify(format!("logical not of {a} operand")));
            }
            stack.push(StackTy::Scalar(ScalarType::Bool));
        }
        Instr::Pop => {
            pop1(stack)?;
        }
        Instr::Dup => {
            let top = *stack
                .last()
                .ok_or_else(|| CodegenError::verify("dup on empty stack"))?;
            stack.push(top);
        }
        Instr::Swap => {
            let len = stack.len();
            if len < 2 {
                return Err(CodegenError::verify("swap needs two operands"));
            }
            stack.swap(len - 1, len - 2);
        }
        Instr::GetLocal(slot) => match slot_types[*slot as usize] {
            Some(ty) => stack.push(StackTy::Scalar(ty)),
            None => {
                if pending_is_error {
                    return Err(CodegenError::verify(format!(
                        "read of local slot {slot} before it is assigned a scalar type"
                    )));
                }
                return Ok(true);
            }
        },
        Instr::DefLocal(slot) | Instr::SetLocal(slot) => {
            let value = pop1(stack)?.require_scalar("a local binding")?;
            let entry = &mut slot_types[*slot as usize];
            match entry {
                Some(existing) if *existing != value => {
                    return Err(CodegenError::verify(format!(
                        "local slot {slot} reused with conflicting types: {existing} vs {value}"
                    )));
                }
                slot_ref => *slot_ref = Some(value),
            }
        }
        Instr::Nop => {}
        // Terminators are handled by the caller.
        Instr::Jump(_) | Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) | Instr::Return => {
            return Err(CodegenError::verify("terminator in block body"));
        }
    }
    Ok(false)
}

fn pop1(stack: &mut TypeStack) -> Result<StackTy, CodegenError> {
    stack
        .pop()
        .ok_or_else(|| CodegenError::verify("operand stack underflow"))
}

fn pop2_scalar(
    stack: &mut TypeStack,
    context: &str,
) -> Result<(ScalarType, ScalarType), CodegenError> {
    let b = pop1(stack)?.require_scalar(context)?;
    let a = pop1(stack)?.require_scalar(context)?;
    Ok((a, b))
}
