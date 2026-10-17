use crate::value::{VmError, VmValue};

#[derive(Clone, Debug)]
pub struct RuntimeContext {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub root_task_id: String,
    pub task_name: Option<String>,
    pub task_group_id: Option<String>,
    pub scope_id: Option<String>,
    pub values: crate::value::DictMap,
}

impl RuntimeContext {
    pub fn root() -> Self {
        Self {
            task_id: "task_root".to_string(),
            parent_task_id: None,
            root_task_id: "task_root".to_string(),
            task_name: Some("root".to_string()),
            task_group_id: None,
            scope_id: None,
            values: crate::value::DictMap::new(),
        }
    }

    pub fn child_task(
        &self,
        task_id: impl Into<String>,
        task_name: impl Into<String>,
        task_group_id: Option<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            parent_task_id: Some(self.task_id.clone()),
            root_task_id: self.root_task_id.clone(),
            task_name: Some(task_name.into()),
            task_group_id,
            scope_id: self.scope_id.clone(),
            values: self.values.clone(),
        }
    }
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self::root()
    }
}

pub fn runtime_context_get(vm: &crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "HarnessRuntime.context_get")?;
    Ok(vm
        .runtime_context
        .values
        .get(key.as_str())
        .cloned()
        .or_else(|| args.get(1).cloned())
        .unwrap_or(VmValue::Nil))
}

pub fn runtime_context_set(vm: &mut crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "HarnessRuntime.context_set")?;
    let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    Ok(vm
        .runtime_context
        .values
        .insert(crate::value::intern_key(&key), value)
        .unwrap_or(VmValue::Nil))
}

pub fn runtime_context_clear(vm: &mut crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "HarnessRuntime.context_clear")?;
    Ok(vm
        .runtime_context
        .values
        .remove(key.as_str())
        .unwrap_or(VmValue::Nil))
}

fn require_key(args: &[VmValue], builtin: &str) -> Result<String, VmError> {
    match args.first() {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        _ => Err(VmError::Runtime(format!(
            "{builtin}: first argument must be a string key"
        ))),
    }
}
