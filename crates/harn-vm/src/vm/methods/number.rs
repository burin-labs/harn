use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_number_method(
        &mut self,
        _obj: &VmValue,
        _method: &str,
        _args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Ok(VmValue::Nil)
    }
}
