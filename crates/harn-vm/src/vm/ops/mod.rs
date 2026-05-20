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
    /// Execute a single opcode in a non-`async` context.
    ///
    /// Returns `Some(result)` for sync opcodes (the vast majority — arithmetic,
    /// comparison, jumps, slot ops, etc.) and `None` for opcodes that must
    /// `.await` (calls, method dispatch, iter-next, pipe, parallel families,
    /// imports, yield). The interpreter's hot loop tries this first to skip
    /// the per-iteration future-state-machine overhead that the unified async
    /// dispatcher used to pay on every opcode.
    ///
    /// Coverage parity with [`execute_op_async`] is enforced by the
    /// exhaustive match: a newly added opcode forces a `match` update and the
    /// fall-through arm classifies it as sync-vs-async explicitly.
    pub(super) fn execute_op_sync(&mut self, op: Op) -> Option<Result<(), VmError>> {
        let result: Result<(), VmError> = match op {
            Op::Constant => self.execute_constant(),
            Op::Nil => {
                self.execute_nil();
                Ok(())
            }
            Op::True => {
                self.execute_true();
                Ok(())
            }
            Op::False => {
                self.execute_false();
                Ok(())
            }
            Op::GetVar => self.execute_get_var(),
            Op::DefLet => self.execute_def_let(),
            Op::DefVar => self.execute_def_var(),
            Op::SetVar => self.execute_set_var(),
            Op::GetLocalSlot => self.execute_get_local_slot(),
            Op::DefLocalSlot => self.execute_def_local_slot(),
            Op::SetLocalSlot => self.execute_set_local_slot(),
            Op::PushScope => {
                self.execute_push_scope();
                Ok(())
            }
            Op::PopScope => {
                self.execute_pop_scope();
                Ok(())
            }
            Op::Add => self.execute_add(),
            Op::Sub => self.execute_sub(),
            Op::Mul => self.execute_mul(),
            Op::Div => self.execute_div(),
            Op::Mod => self.execute_mod(),
            Op::Pow => self.execute_pow(),
            Op::Negate => self.execute_negate(),
            Op::Equal => self.execute_equal(),
            Op::NotEqual => self.execute_not_equal(),
            Op::Less => self.execute_less(),
            Op::Greater => self.execute_greater(),
            Op::LessEqual => self.execute_less_equal(),
            Op::GreaterEqual => self.execute_greater_equal(),
            Op::Not => self.execute_not(),
            Op::Jump => {
                self.execute_jump();
                Ok(())
            }
            Op::JumpIfFalse => self.execute_jump_if_false(),
            Op::JumpIfTrue => self.execute_jump_if_true(),
            Op::Pop => self.execute_pop(),
            Op::Return => return Some(Err(self.execute_return())),
            Op::Closure => {
                self.execute_closure();
                Ok(())
            }
            Op::BuildList => {
                self.execute_build_list();
                Ok(())
            }
            Op::BuildDict => {
                self.execute_build_dict();
                Ok(())
            }
            Op::Subscript => self.execute_subscript(false),
            Op::SubscriptOpt => self.execute_subscript(true),
            Op::Slice => self.execute_slice(),
            Op::GetProperty => self.execute_get_property(false),
            Op::GetPropertyOpt => self.execute_get_property(true),
            Op::SetProperty => self.execute_set_property(),
            Op::SetSubscript => self.execute_set_subscript(),
            Op::Concat => {
                self.execute_concat();
                Ok(())
            }
            Op::IterInit => self.execute_iter_init(),
            // IterNext is a split opcode: the sync fast path handles
            // Vec/Dict/Range/empty iterators inline; if it returns `None`
            // we hand off to `execute_op_async` for Channel/Generator/
            // Stream/VmIter without having touched `ip`.
            Op::IterNext => return self.execute_iter_next_sync(),
            // Call is split: the sync fast path handles non-generator
            // user closures with no `@step` definition attached. Other
            // callee variants (string, builtin ref, etc.) return `None`
            // and fall through to `execute_call_async` without touching
            // `ip`.
            Op::Call => return self.execute_call_sync(),
            // CallBuiltin is the opcode `f(x)` compiles to and the
            // actual hot dispatch for user closures. The sync fast path
            // peeks the name from the inline operand, skips
            // runtime-construct names (`await`/`cancel`/...), generators,
            // and `@step`-decorated functions, then pushes the closure
            // frame inline. Builtins and the listed escape hatches fall
            // through to `execute_call_builtin_async` with `ip` untouched.
            Op::CallBuiltin => return self.execute_call_builtin_sync(),
            // TailCall is split: the sync fast path handles the
            // steady-state user-closure tail call inline (TCO frame
            // reuse). Tracked-function frames/callees, generators, and
            // string/builtin-ref callees return `None` and fall through
            // to `execute_tail_call_async`.
            Op::TailCall => return self.execute_tail_call_sync(),
            Op::Throw => self.execute_throw(),
            Op::TryCatchSetup => {
                self.execute_try_catch_setup();
                Ok(())
            }
            Op::PopHandler => {
                self.execute_pop_handler();
                Ok(())
            }
            Op::Spawn => self.execute_spawn(),
            Op::DeadlineSetup => self.execute_deadline_setup(),
            Op::DeadlineEnd => {
                self.execute_deadline_end();
                Ok(())
            }
            Op::BuildEnum => self.execute_build_enum(),
            Op::MatchEnum => self.execute_match_enum(),
            Op::PopIterator => {
                self.execute_pop_iterator();
                Ok(())
            }
            Op::GetArgc => {
                self.execute_get_argc();
                Ok(())
            }
            Op::CheckType => self.execute_check_type(),
            Op::TryUnwrap => self.execute_try_unwrap(),
            Op::TryWrapOk => self.execute_try_wrap_ok(),
            Op::Dup => self.execute_dup(),
            Op::Swap => {
                self.execute_swap();
                Ok(())
            }
            Op::Contains => self.execute_contains(),
            Op::AddInt => self.execute_add_int(),
            Op::SubInt => self.execute_sub_int(),
            Op::MulInt => self.execute_mul_int(),
            Op::DivInt => self.execute_div_int(),
            Op::ModInt => self.execute_mod_int(),
            Op::AddFloat => self.execute_add_float(),
            Op::SubFloat => self.execute_sub_float(),
            Op::MulFloat => self.execute_mul_float(),
            Op::DivFloat => self.execute_div_float(),
            Op::ModFloat => self.execute_mod_float(),
            Op::EqualInt => self.execute_equal_int(),
            Op::NotEqualInt => self.execute_not_equal_int(),
            Op::LessInt => self.execute_less_int(),
            Op::GreaterInt => self.execute_greater_int(),
            Op::LessEqualInt => self.execute_less_equal_int(),
            Op::GreaterEqualInt => self.execute_greater_equal_int(),
            Op::EqualFloat => self.execute_equal_float(),
            Op::NotEqualFloat => self.execute_not_equal_float(),
            Op::LessFloat => self.execute_less_float(),
            Op::GreaterFloat => self.execute_greater_float(),
            Op::LessEqualFloat => self.execute_less_equal_float(),
            Op::GreaterEqualFloat => self.execute_greater_equal_float(),
            Op::EqualBool => self.execute_equal_bool(),
            Op::NotEqualBool => self.execute_not_equal_bool(),
            Op::EqualString => self.execute_equal_string(),
            Op::NotEqualString => self.execute_not_equal_string(),
            // Async-dispatched opcodes: caller must fall through to
            // `execute_op_async`. Keeping these in a single explicit arm
            // keeps the sync/async classification visible alongside the
            // sync dispatch table.
            Op::CallBuiltinSpread
            | Op::MethodCall
            | Op::MethodCallOpt
            | Op::Pipe
            | Op::Parallel
            | Op::ParallelMap
            | Op::ParallelMapStream
            | Op::ParallelSettle
            | Op::SyncMutexEnter
            | Op::Import
            | Op::SelectiveImport
            | Op::CallSpread
            | Op::MethodCallSpread
            | Op::Yield => return None,
        };
        Some(result)
    }

    /// Execute a single async opcode. The caller must have already verified
    /// that [`execute_op_sync`] returned `None` for this opcode; reaching the
    /// catch-all is a coverage bug between the two dispatch tables.
    pub(super) async fn execute_op_async(&mut self, op: Op) -> Result<(), VmError> {
        match op {
            Op::Call => self.execute_call_async().await,
            Op::CallBuiltin => self.execute_call_builtin_async().await,
            Op::CallBuiltinSpread => self.execute_call_builtin_spread().await,
            Op::TailCall => self.execute_tail_call_async().await,
            Op::MethodCall => self.execute_method_call(false).await,
            Op::MethodCallOpt => self.execute_method_call(true).await,
            Op::IterNext => self.execute_iter_next_async().await,
            Op::Pipe => self.execute_pipe().await,
            Op::Parallel => self.execute_parallel().await,
            Op::ParallelMap => self.execute_parallel_map().await,
            Op::ParallelMapStream => self.execute_parallel_map_stream().await,
            Op::ParallelSettle => self.execute_parallel_settle().await,
            Op::SyncMutexEnter => self.execute_sync_mutex_enter().await,
            Op::Import => self.execute_import_op().await,
            Op::SelectiveImport => self.execute_selective_import().await,
            Op::CallSpread => self.execute_call_spread().await,
            Op::MethodCallSpread => self.execute_method_call_spread().await,
            Op::Yield => self.execute_yield().await,
            sync_op => {
                debug_assert!(
                    false,
                    "execute_op_async called with sync opcode {sync_op:?} — \
                     dispatch tables in execute_op_sync / execute_op_async are out of sync"
                );
                Err(VmError::Runtime(format!(
                    "internal VM dispatch error: {sync_op:?} is not an async opcode"
                )))
            }
        }
    }

    /// Execute a single opcode. Used by the scope-interrupt wrapper that
    /// drives cancellable / deadlined execution; the hot interpreter loop
    /// in `run_chunk_ref` bypasses this and calls `execute_op_sync` /
    /// `execute_op_async` directly when no interrupt machinery is armed.
    pub(super) async fn execute_op(&mut self, op_byte: u8) -> Result<Option<VmValue>, VmError> {
        let op = Op::from_byte(op_byte).ok_or(VmError::InvalidInstruction(op_byte))?;
        if let Some(result) = self.execute_op_sync(op) {
            result?;
            return Ok(None);
        }
        self.execute_op_async(op).await?;
        Ok(None)
    }
}
