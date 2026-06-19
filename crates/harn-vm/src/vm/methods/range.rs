use crate::value::{VmError, VmRange, VmValue};
use crate::vm::iter::iter_from_value;

impl crate::vm::Vm {
    pub(super) fn call_range_method_sync(
        obj: &VmValue,
        r: &VmRange,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let result = match method {
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
            "to_string" => Ok(VmValue::String(arcstr::ArcStr::from(obj.display()))),
            _ => return None,
        };
        Some(result)
    }

    pub(super) async fn call_range_method(
        &mut self,
        obj: &VmValue,
        r: &VmRange,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(result) = Self::call_range_method_sync(obj, r, method, args) {
            return result;
        }

        let lifted = iter_from_value(obj.clone())?;
        match lifted {
            VmValue::Iter(handle) => self.call_iter_method(&handle, method, args).await,
            other => Err(VmError::TypeError(format!(
                "value of type {} has no method `{method}`",
                other.type_name()
            ))),
        }
    }
}
