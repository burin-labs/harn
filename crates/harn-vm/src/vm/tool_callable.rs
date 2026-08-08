use std::sync::Arc;

use crate::value::{DictMap, VmClosure, VmError, VmValue};

use super::Vm;

pub(crate) fn single_harn_tool_handler(
    registry: &DictMap,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    match registry.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => {}
        _ => return Ok(None),
    }

    let tools = registry_tools(registry);
    let [tool] = tools else {
        return Err(VmError::TypeError(format!(
            "Cannot call tool registry with {} tools; expected exactly one Harn-backed tool",
            tools.len()
        )));
    };
    let VmValue::Dict(entry) = tool else {
        return Err(VmError::TypeError(
            "Cannot call malformed tool registry: tool entry is not a dict".to_string(),
        ));
    };

    let name = entry
        .get("name")
        .map(VmValue::display)
        .unwrap_or_else(|| "<unnamed>".to_string());
    let executor = entry
        .get("executor")
        .map(VmValue::display)
        .unwrap_or_else(|| "harn".to_string());
    if executor != "harn" {
        return Err(VmError::TypeError(format!(
            "Cannot call tool registry entry {name:?}: executor {executor:?} is not Harn-backed"
        )));
    }

    match entry.get("handler") {
        Some(VmValue::Closure(handler)) => Ok(Some(Arc::clone(handler))),
        _ => Err(VmError::TypeError(format!(
            "Cannot call tool registry entry {name:?}: missing Harn closure handler"
        ))),
    }
}

pub(crate) fn single_harn_tool_handler_value(
    value: &VmValue,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    match value {
        VmValue::Dict(registry) => single_harn_tool_handler(registry),
        _ => Ok(None),
    }
}

pub(crate) fn resolve_named_single_harn_tool_handler(
    vm: &Vm,
    name: &str,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    let Some(value) = vm.resolve_named_value(name) else {
        return Ok(None);
    };
    single_harn_tool_handler_value(&value)
}

pub(crate) fn is_single_harn_tool_registry_value(value: &VmValue) -> bool {
    single_harn_tool_handler_value(value)
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn require_single_harn_tool_handler(
    registry: &DictMap,
    fallback: impl FnOnce() -> String,
) -> Result<Arc<VmClosure>, VmError> {
    single_harn_tool_handler(registry)?.ok_or_else(|| VmError::TypeError(fallback()))
}

fn registry_tools(dict: &DictMap) -> &[VmValue] {
    match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

impl Vm {
    pub(in crate::vm) async fn call_user_closure_from_stack_args(
        &mut self,
        closure: Arc<VmClosure>,
        args_start: usize,
        stack_truncate_to: usize,
    ) -> Result<(), VmError> {
        if closure.func.is_generator
            || crate::step_runtime::step_definition_for_function(&closure.func.name).is_some()
        {
            let args = self.take_stack_args_from(args_start)?;
            self.stack.truncate(stack_truncate_to);
            self.call_user_closure(closure, args).await?;
        } else {
            self.push_closure_frame_from_stack_args(&closure, args_start, stack_truncate_to)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    fn registry(tools: Vec<VmValue>) -> DictMap {
        let mut registry = DictMap::new();
        registry.put_str("_type", "tool_registry");
        registry.insert(
            crate::value::intern_key("tools"),
            VmValue::List(Arc::new(tools)),
        );
        registry
    }

    fn tool_entry(name: &str, executor: &str) -> VmValue {
        let mut entry = DictMap::new();
        entry.put_str("name", name);
        entry.put_str("executor", executor);
        entry.insert(crate::value::intern_key("handler"), VmValue::Nil);
        VmValue::dict(entry)
    }

    #[test]
    fn single_harn_tool_handler_ignores_non_registry_dicts() {
        assert!(single_harn_tool_handler(&DictMap::new()).unwrap().is_none());
    }

    #[test]
    fn single_harn_tool_handler_rejects_multi_tool_registries() {
        let err = single_harn_tool_handler(&registry(vec![
            tool_entry("first", "harn"),
            tool_entry("second", "harn"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("with 2 tools"), "{err}");
    }

    #[test]
    fn single_harn_tool_handler_rejects_external_executors() {
        let err = single_harn_tool_handler(&registry(vec![tool_entry("edit", "host_bridge")]))
            .unwrap_err();
        assert!(err.to_string().contains("not Harn-backed"), "{err}");
    }
}
