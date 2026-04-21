use crate::chunk::CompiledFunction;
use crate::value::{VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_struct_instance_method(
        &mut self,
        obj: &VmValue,
        method: &str,
        args: &[VmValue],
        functions: &[CompiledFunction],
    ) -> Result<VmValue, VmError> {
        let VmValue::StructInstance { layout, .. } = obj else {
            unreachable!("struct instance dispatch only calls struct instance handler");
        };

        let impl_key = format!("__impl_{}", layout.struct_name());
        if let Some(VmValue::Dict(impl_dict)) = self.env.get(&impl_key) {
            if let Some(VmValue::Closure(closure)) = impl_dict.get(method) {
                let mut full_args = vec![obj.clone()];
                full_args.extend_from_slice(args);
                return self.call_closure(closure, &full_args, functions).await;
            }
        }

        Ok(VmValue::Nil)
    }
}
