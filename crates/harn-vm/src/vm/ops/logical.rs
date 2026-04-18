use crate::chunk::Op;
use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn try_execute_logical_op(&mut self, op: u8) -> Result<bool, VmError> {
        if op == Op::Not as u8 {
            let v = self.pop()?;
            self.stack.push(VmValue::Bool(!v.is_truthy()));
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
