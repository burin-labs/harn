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

use crate::value::{VmError, VmValue};

impl super::Vm {
    /// Execute a single opcode. Returns:
    /// - Ok(None): continue execution
    /// - Ok(Some(val)): return this value (top-level exit)
    /// - Err(e): error occurred
    pub(super) async fn execute_op(&mut self, op: u8) -> Result<Option<VmValue>, VmError> {
        if self.try_execute_stack_op(op)? {
            return Ok(None);
        }
        if self.try_execute_arithmetic_op(op)? {
            return Ok(None);
        }
        if self.try_execute_comparison_op(op)? {
            return Ok(None);
        }
        if self.try_execute_logical_op(op)? {
            return Ok(None);
        }
        if self.try_execute_control_flow_op(op)? {
            return Ok(None);
        }
        if self.try_execute_call_op(op).await? {
            return Ok(None);
        }
        if self.try_execute_collections_op(op)? {
            return Ok(None);
        }
        if self.try_execute_iter_op(op).await? {
            return Ok(None);
        }
        if self.try_execute_parallel_op(op).await? {
            return Ok(None);
        }
        if self.try_execute_exception_op(op)? {
            return Ok(None);
        }
        if self.try_execute_imports_op(op).await? {
            return Ok(None);
        }
        if self.try_execute_misc_op(op).await? {
            return Ok(None);
        }
        Err(VmError::InvalidInstruction(op))
    }
}
