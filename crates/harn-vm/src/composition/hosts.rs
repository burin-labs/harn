use std::collections::BTreeMap;

use serde_json::Value;

use crate::agent_events::ToolCallErrorCategory;
use crate::value::VmValue;
use crate::vm::AsyncBuiltinCtx;
use crate::VmClosure;

use super::manifest::BindingManifestEntry;
use super::types::{CompositionToolHost, CompositionToolOutput};

pub struct StaticCompositionToolHost {
    outputs: BTreeMap<String, Value>,
}

impl StaticCompositionToolHost {
    pub fn new(outputs: BTreeMap<String, Value>) -> Self {
        Self { outputs }
    }
}

#[async_trait::async_trait]
impl CompositionToolHost for StaticCompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        if let Some(value) = self.outputs.get(&binding.name) {
            return CompositionToolOutput::ok(value.clone());
        }
        if let Some(value) = binding.metadata.get("mock_output") {
            return CompositionToolOutput::ok(value.clone());
        }
        CompositionToolOutput::ok(serde_json::json!({
            "tool": binding.name,
            "input": input,
        }))
    }
}

/// Dispatches every binding call to a caller-supplied Harn closure.
///
/// The closure is compiled in the calling VM and receives
/// `(binding_name: string, input: dict)`. Returning a value yields a successful
/// child result; raising an error fails the child call. Each invocation runs
/// on a fresh clone of the outer VM so closure-side builtins (`host_call`,
/// pipeline imports, etc.) resolve normally — the inner composition VM only
/// sees the manifest bindings plus pure helpers.
pub struct ClosureCompositionToolHost {
    closure: VmClosure,
    ctx: AsyncBuiltinCtx,
}

impl ClosureCompositionToolHost {
    pub fn new(closure: VmClosure, ctx: AsyncBuiltinCtx) -> Self {
        Self { closure, ctx }
    }
}

#[async_trait::async_trait]
impl CompositionToolHost for ClosureCompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        let mut vm = self.ctx.child_vm();
        crate::stdlib::host::register_missing_host_builtins(&mut vm);
        let args = vec![
            VmValue::String(arcstr::ArcStr::from(binding.name.as_str())),
            crate::json_to_vm_value(&input),
        ];
        let result = vm.call_closure_pub(&self.closure, &args).await;
        self.ctx.forward_output(&vm.take_output());
        match result {
            Ok(value) => {
                let json = crate::llm::vm_value_to_json(&value);
                CompositionToolOutput::ok(json)
            }
            Err(error) => {
                CompositionToolOutput::error(error.to_string(), ToolCallErrorCategory::ToolError)
            }
        }
    }
}
