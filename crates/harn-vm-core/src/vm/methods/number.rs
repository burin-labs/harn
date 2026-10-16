use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) fn call_number_method(
        obj: &VmValue,
        method: &str,
        _args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        // Numbers expose no methods. Returning `nil` for every call silently
        // swallowed real mistakes (`(3.14).round(2)` used to yield `nil`);
        // throw like every other receiver type so a missing method is a
        // catchable error instead of an untyped `nil`.
        Err(VmError::Runtime(format!(
            "value of type {} has no method `{method}`",
            obj.type_name()
        )))
    }
}
