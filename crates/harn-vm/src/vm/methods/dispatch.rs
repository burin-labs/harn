use std::future::Future;
use std::pin::Pin;

use crate::value::{VmError, VmValue};
use crate::vm::iter::iter_from_value;

impl crate::vm::Vm {
    pub(in crate::vm) fn call_method_sync(
        obj: &VmValue,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if method == "iter"
            && matches!(
                obj,
                VmValue::List(_)
                    | VmValue::Set(_)
                    | VmValue::Dict(_)
                    | VmValue::String(_)
                    | VmValue::Generator(_)
                    | VmValue::Stream(_)
                    | VmValue::Channel(_)
                    | VmValue::Iter(_)
            )
        {
            return Some(iter_from_value(obj.clone()));
        }

        match obj {
            VmValue::String(s) => Some(Self::call_string_method(s, method, args)),
            VmValue::List(items) => Self::call_list_method_sync(items, method, args),
            VmValue::Dict(map) => Self::call_dict_method_sync(map, method, args),
            VmValue::Set(items) => Self::call_set_method_sync(items, method, args),
            VmValue::Int(_) | VmValue::Float(_) => {
                Some(Self::call_number_method(obj, method, args))
            }
            VmValue::Range(r) => Self::call_range_method_sync(obj, r, method, args),
            _ => None,
        }
    }

    pub(in crate::vm) fn call_method_async<'a>(
        &'a mut self,
        obj: VmValue,
        method: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            if let Some(result) = Self::call_method_sync(&obj, method, args) {
                return result;
            }

            match &obj {
                VmValue::List(items) => self.call_list_method(items, method, args).await,
                VmValue::Dict(map) => self.call_dict_method(map, method, args).await,
                VmValue::Set(items) => self.call_set_method(items, method, args).await,
                VmValue::Range(r) => self.call_range_method(&obj, r, method, args).await,
                VmValue::StructInstance { .. } => {
                    self.call_struct_instance_method(&obj, method, args).await
                }
                VmValue::BuiltinRef(name) => {
                    let qualified = format!("{name}.{method}");
                    if self.builtins.contains_key(&qualified)
                        || self.async_builtins.contains_key(&qualified)
                    {
                        self.call_named_builtin(&qualified, args.to_vec()).await
                    } else {
                        Err(VmError::TypeError(format!(
                            "value of type {} has no method `{method}`",
                            obj.type_name()
                        )))
                    }
                }
                VmValue::BuiltinRefId { name, .. } => {
                    let qualified = format!("{name}.{method}");
                    if self.builtins.contains_key(&qualified)
                        || self.async_builtins.contains_key(&qualified)
                    {
                        self.call_named_builtin(&qualified, args.to_vec()).await
                    } else {
                        Err(VmError::TypeError(format!(
                            "value of type {} has no method `{method}`",
                            obj.type_name()
                        )))
                    }
                }
                VmValue::Generator(gen) => self.call_generator_method(gen, method).await,
                VmValue::Stream(stream) => self.call_stream_method(stream, method).await,
                VmValue::Iter(handle) => self.call_iter_method(handle, method, args).await,
                VmValue::Harness(handle) => {
                    let handle = handle.clone();
                    self.call_harness_method(&handle, method, args).await
                }
                other => Err(VmError::TypeError(format!(
                    "value of type {} has no method `{method}`",
                    other.type_name()
                ))),
            }
        })
    }
}
