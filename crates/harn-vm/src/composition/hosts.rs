use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;

use crate::agent_events::ToolCallErrorCategory;
use crate::value::VmValue;
use crate::vm::Vm;
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

#[async_trait::async_trait(?Send)]
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
    outer_vm: Vm,
}

impl ClosureCompositionToolHost {
    pub fn new(closure: VmClosure, outer_vm: Vm) -> Self {
        Self { closure, outer_vm }
    }
}

#[async_trait::async_trait(?Send)]
impl CompositionToolHost for ClosureCompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        let mut vm = self.outer_vm.child_vm();
        let args = vec![
            VmValue::String(Rc::from(binding.name.as_str())),
            crate::json_to_vm_value(&input),
        ];
        match vm.call_closure_pub(&self.closure, &args).await {
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
