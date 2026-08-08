use crate::value::{VmError, VmValue};

impl super::super::Vm {
    pub(super) fn execute_not(&mut self) -> Result<(), VmError> {
        let v = self.pop()?;
        self.stack.push(VmValue::Bool(!v.is_truthy()));
        Ok(())
    }
}
