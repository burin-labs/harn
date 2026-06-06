//! Lowering of a verified [`ScalarFunction`] to Cranelift IR.
//!
//! # Calling convention
//!
//! Every compiled function is emitted with one uniform C signature regardless
//! of its Harn arity or types:
//!
//! ```text
//! extern "C" fn(args: *const u64, ret: *mut u64, trap: *mut u8)
//! ```
//!
//! Arguments and the result are passed as raw 64-bit slots (see
//! [`ScalarValue::to_bits`]). This keeps the Rust ↔ native boundary a single
//! monomorphic function pointer — no per-signature transmute zoo — and makes
//! the AOT object expose one stable, easily-called symbol.
//!
//! Integer `/` and `%` by zero set `*trap = 1` and return, mirroring the
//! interpreter's runtime error instead of executing a hardware trap that would
//! abort the host process.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, BlockArg, Function, InstBuilder, MemFlags, Signature, Type, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{FuncId, Linkage, Module};

use crate::bytecode::{BinOp, CmpOp, Instr};
use crate::error::CodegenError;
use crate::value::{ScalarType, ScalarValue};
use crate::verify::{ScalarFunction, Terminator};

/// Number of pointer parameters in the uniform ABI: `args`, `ret`, `trap`.
const ABI_PARAM_COUNT: usize = 3;

/// Fill in `sig` with the uniform native calling convention for `ptr_ty`.
pub(crate) fn build_signature(sig: &mut Signature, ptr_ty: Type) {
    sig.params.clear();
    sig.returns.clear();
    for _ in 0..ABI_PARAM_COUNT {
        sig.params.push(AbiParam::new(ptr_ty));
    }
}

/// Declare and define `sf` as an exported function named `symbol` in any
/// Cranelift [`Module`] (JIT or object). Shared by both backends so the
/// signature, lowering, and error mapping stay in one place.
pub(crate) fn define_scalar_function<M: Module>(
    module: &mut M,
    sf: &ScalarFunction,
    symbol: &str,
) -> Result<FuncId, CodegenError> {
    let ptr_ty = module.target_config().pointer_type();
    let mut sig = module.make_signature();
    build_signature(&mut sig, ptr_ty);

    let id = module
        .declare_function(symbol, Linkage::Export, &sig)
        .map_err(|e| CodegenError::backend(e.to_string()))?;

    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let mut fb_ctx = FunctionBuilderContext::new();
    lower(&mut ctx.func, &mut fb_ctx, sf)?;
    module
        .define_function(id, &mut ctx)
        .map_err(|e| CodegenError::backend(e.to_string()))?;
    module.clear_context(&mut ctx);
    Ok(id)
}

/// Cranelift type for a scalar: `int` → `i64`, `bool` → `i8`, `float` → `f64`.
fn clif_ty(ty: ScalarType) -> Type {
    match ty {
        ScalarType::Int => types::I64,
        ScalarType::Bool => types::I8,
        ScalarType::Float => types::F64,
    }
}

/// Lower `sf` into `func` (whose signature must already be set via
/// [`build_signature`]).
pub(crate) fn lower(
    func: &mut Function,
    fb_ctx: &mut FunctionBuilderContext,
    sf: &ScalarFunction,
) -> Result<(), CodegenError> {
    let mut builder = FunctionBuilder::new(func, fb_ctx);

    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let abi = builder.block_params(entry).to_vec();
    let args_ptr = abi[0];
    let ret_ptr = abi[1];
    let trap_ptr = abi[2];

    // Declare one Cranelift variable per local slot; initialise parameters
    // from the args array and everything else to a typed zero. The verifier
    // guarantees non-parameter slots are written before any meaningful read,
    // so the zero is never observed — it just keeps the SSA builder happy.
    let mut vars = Vec::with_capacity(sf.slot_count());
    for slot_ty in &sf.slot_types {
        vars.push(builder.declare_var(clif_ty(*slot_ty)));
    }
    for (idx, slot_ty) in sf.slot_types.iter().enumerate() {
        let value = if idx < sf.params.len() {
            load_arg(&mut builder, args_ptr, idx, *slot_ty)
        } else {
            zero_of(&mut builder, *slot_ty)
        };
        builder.def_var(vars[idx], value);
    }

    // Clear the trap flag up front; only the trap block ever sets it.
    let zero8 = builder.ins().iconst(types::I8, 0);
    builder.ins().store(MemFlags::trusted(), zero8, trap_ptr, 0);

    // One Cranelift block per scalar block, carrying the operand-stack shape
    // as block parameters.
    let clif_blocks: Vec<_> = sf
        .blocks
        .iter()
        .map(|block| {
            let clb = builder.create_block();
            for ty in &block.stack_in {
                builder.append_block_param(clb, clif_ty(*ty));
            }
            clb
        })
        .collect();

    // Shared block for the divide-by-zero trap path.
    let trap_block = builder.create_block();

    // Entry falls into block 0 (whose entry stack is empty).
    let no_args: Vec<BlockArg> = Vec::new();
    builder.ins().jump(clif_blocks[0], &no_args);

    for (idx, block) in sf.blocks.iter().enumerate() {
        builder.switch_to_block(clif_blocks[idx]);
        let mut stack: Vec<Value> = builder.block_params(clif_blocks[idx]).to_vec();
        for instr in &block.body {
            lower_instr(&mut builder, &vars, &mut stack, instr, trap_block);
        }
        match block.term {
            Terminator::Return => {
                let value = stack.pop().expect("verified non-empty return stack");
                store_ret(&mut builder, ret_ptr, sf.ret, value);
                builder.ins().return_(&[]);
            }
            Terminator::Jump(target) => {
                let args = block_args(&stack);
                builder.ins().jump(clif_blocks[target], &args);
            }
            Terminator::Branch { on_true, on_false } => {
                let cond = *stack.last().expect("verified non-empty branch stack");
                let args = block_args(&stack);
                builder.ins().brif(
                    cond,
                    clif_blocks[on_true],
                    &args,
                    clif_blocks[on_false],
                    &args,
                );
            }
        }
    }

    // Emit the trap path: set *trap = 1, write a zero result, return.
    builder.switch_to_block(trap_block);
    let one8 = builder.ins().iconst(types::I8, 1);
    builder.ins().store(MemFlags::trusted(), one8, trap_ptr, 0);
    let zero_ret = zero_of(&mut builder, sf.ret);
    store_ret(&mut builder, ret_ptr, sf.ret, zero_ret);
    builder.ins().return_(&[]);

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn lower_instr(
    builder: &mut FunctionBuilder,
    vars: &[Variable],
    stack: &mut Vec<Value>,
    instr: &Instr,
    trap_block: cranelift_codegen::ir::Block,
) {
    match instr {
        Instr::Const(value) => {
            let v = const_value(builder, *value);
            stack.push(v);
        }
        Instr::Bin(op) => {
            let b = stack.pop().unwrap();
            let a = stack.pop().unwrap();
            let result = lower_bin(builder, *op, a, b, trap_block);
            stack.push(result);
        }
        Instr::Cmp(op) => {
            let b = stack.pop().unwrap();
            let a = stack.pop().unwrap();
            stack.push(lower_cmp(builder, *op, a, b));
        }
        Instr::Neg => {
            let a = stack.pop().unwrap();
            let result = if is_float(builder, a) {
                builder.ins().fneg(a)
            } else {
                builder.ins().ineg(a)
            };
            stack.push(result);
        }
        Instr::Not => {
            let a = stack.pop().unwrap();
            stack.push(builder.ins().icmp_imm(IntCC::Equal, a, 0));
        }
        Instr::Pop => {
            stack.pop();
        }
        Instr::Dup => {
            let top = *stack.last().unwrap();
            stack.push(top);
        }
        Instr::Swap => {
            let len = stack.len();
            stack.swap(len - 1, len - 2);
        }
        Instr::GetLocal(slot) => {
            let value = builder.use_var(vars[*slot as usize]);
            stack.push(value);
        }
        Instr::DefLocal(slot) | Instr::SetLocal(slot) => {
            let value = stack.pop().unwrap();
            builder.def_var(vars[*slot as usize], value);
        }
        Instr::Nop => {}
        // The verifier proved this unit is only ever discarded; an `int` zero
        // placeholder keeps the operand stack and any block parameters
        // type-consistent without ever being observed.
        Instr::PushUnit => {
            let placeholder = builder.ins().iconst(types::I64, 0);
            stack.push(placeholder);
        }
        Instr::Jump(_) | Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) | Instr::Return => {
            unreachable!("terminators are not in block bodies")
        }
    }
}

fn const_value(builder: &mut FunctionBuilder, value: ScalarValue) -> Value {
    match value {
        ScalarValue::Int(n) => builder.ins().iconst(types::I64, n),
        ScalarValue::Bool(b) => builder.ins().iconst(types::I8, i64::from(b)),
        ScalarValue::Float(f) => builder.ins().f64const(f),
    }
}

fn lower_bin(
    builder: &mut FunctionBuilder,
    op: BinOp,
    a: Value,
    b: Value,
    trap_block: cranelift_codegen::ir::Block,
) -> Value {
    if is_float(builder, a) {
        return match op {
            BinOp::Add => builder.ins().fadd(a, b),
            BinOp::Sub => builder.ins().fsub(a, b),
            BinOp::Mul => builder.ins().fmul(a, b),
            BinOp::Div => builder.ins().fdiv(a, b),
            BinOp::Mod => unreachable!("float modulo rejected by the verifier"),
        };
    }
    match op {
        BinOp::Add => builder.ins().iadd(a, b),
        BinOp::Sub => builder.ins().isub(a, b),
        BinOp::Mul => builder.ins().imul(a, b),
        BinOp::Div => lower_idiv(builder, a, b, trap_block, true),
        BinOp::Mod => lower_idiv(builder, a, b, trap_block, false),
    }
}

/// Lower integer `/` (`is_div = true`) or `%` (`is_div = false`) with the
/// interpreter's exact wrapping semantics:
///
/// * divisor `0` → branch to the trap block (runtime error, no hardware trap);
/// * `i64::MIN / -1` → `i64::MIN`, and `i64::MIN % -1` → `0` (matching
///   `wrapping_div`/`wrapping_rem`), avoiding Cranelift's overflow trap.
fn lower_idiv(
    builder: &mut FunctionBuilder,
    a: Value,
    b: Value,
    trap_block: cranelift_codegen::ir::Block,
    is_div: bool,
) -> Value {
    let is_zero = builder.ins().icmp_imm(IntCC::Equal, b, 0);
    let cont = builder.create_block();
    let no_args: Vec<BlockArg> = Vec::new();
    builder
        .ins()
        .brif(is_zero, trap_block, &no_args, cont, &no_args);
    builder.switch_to_block(cont);

    let int_min = builder.ins().iconst(types::I64, i64::MIN);
    let neg_one = builder.ins().iconst(types::I64, -1);
    let is_min = builder.ins().icmp(IntCC::Equal, a, int_min);
    let is_neg_one = builder.ins().icmp(IntCC::Equal, b, neg_one);
    let overflow = builder.ins().band(is_min, is_neg_one);
    let one = builder.ins().iconst(types::I64, 1);
    let safe_b = builder.ins().select(overflow, one, b);

    if is_div {
        let quotient = builder.ins().sdiv(a, safe_b);
        // sdiv(MIN, 1) == MIN, so the select restores the wrapped result.
        builder.ins().select(overflow, int_min, quotient)
    } else {
        // srem(MIN, 1) == 0 == wrapping_rem(MIN, -1); no fix-up needed.
        builder.ins().srem(a, safe_b)
    }
}

fn lower_cmp(builder: &mut FunctionBuilder, op: CmpOp, a: Value, b: Value) -> Value {
    if is_float(builder, a) {
        let cc = match op {
            CmpOp::Eq => FloatCC::Equal,
            CmpOp::Ne => FloatCC::NotEqual,
            CmpOp::Lt => FloatCC::LessThan,
            CmpOp::Gt => FloatCC::GreaterThan,
            CmpOp::Le => FloatCC::LessThanOrEqual,
            CmpOp::Ge => FloatCC::GreaterThanOrEqual,
        };
        return builder.ins().fcmp(cc, a, b);
    }
    let cc = match op {
        CmpOp::Eq => IntCC::Equal,
        CmpOp::Ne => IntCC::NotEqual,
        CmpOp::Lt => IntCC::SignedLessThan,
        CmpOp::Gt => IntCC::SignedGreaterThan,
        CmpOp::Le => IntCC::SignedLessThanOrEqual,
        CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
    };
    builder.ins().icmp(cc, a, b)
}

/// Load argument `idx` from the args array and reinterpret it per `ty`.
fn load_arg(builder: &mut FunctionBuilder, args_ptr: Value, idx: usize, ty: ScalarType) -> Value {
    let offset = i32::try_from(idx * 8).expect("argument index fits in i32 offset");
    let raw = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), args_ptr, offset);
    match ty {
        ScalarType::Int => raw,
        ScalarType::Bool => builder.ins().ireduce(types::I8, raw),
        ScalarType::Float => builder.ins().bitcast(types::F64, MemFlags::new(), raw),
    }
}

/// Encode `value` back to its raw 64-bit slot and store it through `ret_ptr`.
fn store_ret(builder: &mut FunctionBuilder, ret_ptr: Value, ty: ScalarType, value: Value) {
    let bits = match ty {
        ScalarType::Int => value,
        ScalarType::Bool => builder.ins().uextend(types::I64, value),
        ScalarType::Float => builder.ins().bitcast(types::I64, MemFlags::new(), value),
    };
    builder.ins().store(MemFlags::trusted(), bits, ret_ptr, 0);
}

fn zero_of(builder: &mut FunctionBuilder, ty: ScalarType) -> Value {
    match ty {
        ScalarType::Int => builder.ins().iconst(types::I64, 0),
        ScalarType::Bool => builder.ins().iconst(types::I8, 0),
        ScalarType::Float => builder.ins().f64const(0.0),
    }
}

fn is_float(builder: &FunctionBuilder, value: Value) -> bool {
    builder.func.dfg.value_type(value) == types::F64
}

/// Convert operand-stack SSA values into block-call arguments.
fn block_args(stack: &[Value]) -> Vec<BlockArg> {
    stack.iter().map(|v| BlockArg::from(*v)).collect()
}
