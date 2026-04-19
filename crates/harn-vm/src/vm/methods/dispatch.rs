use std::future::Future;
use std::pin::Pin;

use crate::chunk::CompiledFunction;
use crate::value::{VmError, VmValue};
use crate::vm::iter::iter_from_value;

impl crate::vm::Vm {
    pub(in crate::vm) fn call_method<'a>(
        &'a mut self,
        obj: VmValue,
        method: &'a str,
        args: &'a [VmValue],
        functions: &'a [CompiledFunction],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            if method == "iter"
                && matches!(
                    &obj,
                    VmValue::List(_)
                        | VmValue::Set(_)
                        | VmValue::Dict(_)
                        | VmValue::String(_)
                        | VmValue::Generator(_)
                        | VmValue::Channel(_)
                        | VmValue::Iter(_)
                )
            {
                return iter_from_value(obj);
            }

            match &obj {
                VmValue::String(s) => self.call_string_method(s, method, args),
                VmValue::List(items) => self.call_list_method(items, method, args, functions).await,
                VmValue::Dict(map) => self.call_dict_method(map, method, args, functions).await,
                VmValue::Set(items) => self.call_set_method(items, method, args, functions).await,
                VmValue::Range(r) => {
                    self.call_range_method(&obj, r, method, args, functions)
                        .await
                }
                VmValue::Int(_) | VmValue::Float(_) => self.call_number_method(&obj, method, args),
                VmValue::StructInstance { .. } => {
                    self.call_struct_instance_method(&obj, method, args, functions)
                        .await
                }
                VmValue::Generator(gen) => self.call_generator_method(gen, method).await,
                VmValue::Iter(handle) => {
                    self.call_iter_method(handle, method, args, functions).await
                }
                _ => Ok(VmValue::Nil),
            }
        })
    }
}
