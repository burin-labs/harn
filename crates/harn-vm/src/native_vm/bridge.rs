//! Native transport adapter for interpreter builtin calls.

use crate::value::{VmError, VmValue};
use std::{future::Future, pin::Pin};

impl crate::vm::VmBuiltinBridge for crate::bridge::HostBridge {
    fn call_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async move {
            let args_json: Vec<serde_json::Value> = args
                .iter()
                .map(crate::value::json::vm_value_to_json)
                .collect();
            let result = self
                .call(
                    "builtin_call",
                    serde_json::json!({"name": name, "args": args_json}),
                )
                .await?;
            Ok(crate::value::json::json_to_vm_value(&result))
        })
    }
}
