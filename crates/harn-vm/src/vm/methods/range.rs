use crate::value::{VmError, VmRange, VmValue};
use crate::vm::iter::iter_from_value;

impl crate::vm::Vm {
    pub(super) async fn call_range_method(
        &mut self,
        obj: &VmValue,
        r: &VmRange,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "len" | "count" => Ok(VmValue::Int(r.len())),
            "empty" => Ok(VmValue::Bool(r.is_empty())),
            "contains" => {
                let needle = args.first().unwrap_or(&VmValue::Nil);
                let result = match needle {
                    VmValue::Int(n) => r.contains(*n),
                    _ => false,
                };
                Ok(VmValue::Bool(result))
            }
            "first" => Ok(r.first().map(VmValue::Int).unwrap_or(VmValue::Nil)),
            "last" => Ok(r.last().map(VmValue::Int).unwrap_or(VmValue::Nil)),
            "to_string" => Ok(VmValue::String(std::rc::Rc::from(obj.display()))),
            _ => {
                let lifted = iter_from_value(obj.clone())?;
                self.call_method(lifted, method, args).await
            }
        }
    }
}
