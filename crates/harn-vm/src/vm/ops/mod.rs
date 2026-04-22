mod arithmetic;
mod call;
mod collections;
mod comparison;
mod control_flow;
mod exception;
mod imports;
mod iter;
mod logical;
mod misc;
mod parallel;
mod stack;

use crate::chunk::Op;
use crate::value::{VmError, VmValue};

impl super::Vm {
    /// Execute a single opcode. Returns:
    /// - Ok(None): continue execution
    /// - Ok(Some(val)): return this value (top-level exit)
    /// - Err(e): error occurred
    pub(super) async fn execute_op(&mut self, op_byte: u8) -> Result<Option<VmValue>, VmError> {
        let op = Op::from_byte(op_byte).ok_or(VmError::InvalidInstruction(op_byte))?;

        match op {
            Op::Constant => self.execute_constant()?,
            Op::Nil => self.execute_nil(),
            Op::True => self.execute_true(),
            Op::False => self.execute_false(),
            Op::GetVar => self.execute_get_var()?,
            Op::DefLet => self.execute_def_let()?,
            Op::DefVar => self.execute_def_var()?,
            Op::SetVar => self.execute_set_var()?,
            Op::PushScope => self.execute_push_scope(),
            Op::PopScope => self.execute_pop_scope(),
            Op::Add => self.execute_add()?,
            Op::Sub => self.execute_sub()?,
            Op::Mul => self.execute_mul()?,
            Op::Div => self.execute_div()?,
            Op::Mod => self.execute_mod()?,
            Op::Pow => self.execute_pow()?,
            Op::Negate => self.execute_negate()?,
            Op::Equal => self.execute_equal()?,
            Op::NotEqual => self.execute_not_equal()?,
            Op::Less => self.execute_less()?,
            Op::Greater => self.execute_greater()?,
            Op::LessEqual => self.execute_less_equal()?,
            Op::GreaterEqual => self.execute_greater_equal()?,
            Op::Not => self.execute_not()?,
            Op::Jump => self.execute_jump(),
            Op::JumpIfFalse => self.execute_jump_if_false()?,
            Op::JumpIfTrue => self.execute_jump_if_true()?,
            Op::Pop => self.execute_pop()?,
            Op::Call => self.execute_call().await?,
            Op::CallBuiltin => self.execute_call_builtin().await?,
            Op::CallBuiltinSpread => self.execute_call_builtin_spread().await?,
            Op::TailCall => self.execute_tail_call().await?,
            Op::Return => return Err(self.execute_return()),
            Op::Closure => self.execute_closure(),
            Op::BuildList => self.execute_build_list(),
            Op::BuildDict => self.execute_build_dict(),
            Op::Subscript => self.execute_subscript()?,
            Op::Slice => self.execute_slice()?,
            Op::GetProperty => self.execute_get_property(false)?,
            Op::GetPropertyOpt => self.execute_get_property(true)?,
            Op::SetProperty => self.execute_set_property()?,
            Op::SetSubscript => self.execute_set_subscript()?,
            Op::MethodCall => self.execute_method_call(false).await?,
            Op::MethodCallOpt => self.execute_method_call(true).await?,
            Op::Concat => self.execute_concat(),
            Op::IterInit => self.execute_iter_init()?,
            Op::IterNext => self.execute_iter_next().await?,
            Op::Pipe => self.execute_pipe().await?,
            Op::Throw => self.execute_throw()?,
            Op::TryCatchSetup => self.execute_try_catch_setup(),
            Op::PopHandler => self.execute_pop_handler(),
            Op::Parallel => self.execute_parallel().await?,
            Op::ParallelMap => self.execute_parallel_map().await?,
            Op::ParallelSettle => self.execute_parallel_settle().await?,
            Op::Spawn => self.execute_spawn()?,
            Op::Import => self.execute_import_op().await?,
            Op::SelectiveImport => self.execute_selective_import().await?,
            Op::DeadlineSetup => self.execute_deadline_setup()?,
            Op::DeadlineEnd => self.execute_deadline_end(),
            Op::BuildEnum => self.execute_build_enum()?,
            Op::MatchEnum => self.execute_match_enum()?,
            Op::PopIterator => self.execute_pop_iterator(),
            Op::GetArgc => self.execute_get_argc(),
            Op::CheckType => self.execute_check_type()?,
            Op::TryUnwrap => self.execute_try_unwrap()?,
            Op::CallSpread => self.execute_call_spread().await?,
            Op::MethodCallSpread => self.execute_method_call_spread().await?,
            Op::Dup => self.execute_dup()?,
            Op::Swap => self.execute_swap(),
            Op::Contains => self.execute_contains()?,
            Op::AddInt => self.execute_add_int()?,
            Op::SubInt => self.execute_sub_int()?,
            Op::MulInt => self.execute_mul_int()?,
            Op::DivInt => self.execute_div_int()?,
            Op::ModInt => self.execute_mod_int()?,
            Op::AddFloat => self.execute_add_float()?,
            Op::SubFloat => self.execute_sub_float()?,
            Op::MulFloat => self.execute_mul_float()?,
            Op::DivFloat => self.execute_div_float()?,
            Op::ModFloat => self.execute_mod_float()?,
            Op::EqualInt => self.execute_equal_int()?,
            Op::NotEqualInt => self.execute_not_equal_int()?,
            Op::LessInt => self.execute_less_int()?,
            Op::GreaterInt => self.execute_greater_int()?,
            Op::LessEqualInt => self.execute_less_equal_int()?,
            Op::GreaterEqualInt => self.execute_greater_equal_int()?,
            Op::EqualFloat => self.execute_equal_float()?,
            Op::NotEqualFloat => self.execute_not_equal_float()?,
            Op::LessFloat => self.execute_less_float()?,
            Op::GreaterFloat => self.execute_greater_float()?,
            Op::LessEqualFloat => self.execute_less_equal_float()?,
            Op::GreaterEqualFloat => self.execute_greater_equal_float()?,
            Op::EqualBool => self.execute_equal_bool()?,
            Op::NotEqualBool => self.execute_not_equal_bool()?,
            Op::EqualString => self.execute_equal_string()?,
            Op::NotEqualString => self.execute_not_equal_string()?,
            Op::Yield => self.execute_yield().await?,
        }

        Ok(None)
    }
}
