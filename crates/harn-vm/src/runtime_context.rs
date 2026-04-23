use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

#[derive(Clone, Debug)]
pub struct RuntimeContext {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub root_task_id: String,
    pub task_name: Option<String>,
    pub task_group_id: Option<String>,
    pub scope_id: Option<String>,
    pub values: BTreeMap<String, VmValue>,
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
            values: BTreeMap::new(),
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

#[derive(Clone, Debug, Default)]
pub struct RuntimeContextOverlay {
    pub workflow_id: Option<String>,
    pub run_id: Option<String>,
    pub stage_id: Option<String>,
    pub worker_id: Option<String>,
}

thread_local! {
    static RUNTIME_CONTEXT_OVERLAY_STACK: RefCell<Vec<RuntimeContextOverlay>> =
        const { RefCell::new(Vec::new()) };
}

pub struct RuntimeContextOverlayGuard;

pub fn install_runtime_context_overlay(
    overlay: RuntimeContextOverlay,
) -> RuntimeContextOverlayGuard {
    RUNTIME_CONTEXT_OVERLAY_STACK.with(|stack| stack.borrow_mut().push(overlay));
    RuntimeContextOverlayGuard
}

impl Drop for RuntimeContextOverlayGuard {
    fn drop(&mut self) {
        RUNTIME_CONTEXT_OVERLAY_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

fn current_overlay() -> RuntimeContextOverlay {
    RUNTIME_CONTEXT_OVERLAY_STACK.with(|stack| {
        let mut merged = RuntimeContextOverlay::default();
        for overlay in stack.borrow().iter() {
            if overlay.workflow_id.is_some() {
                merged.workflow_id = overlay.workflow_id.clone();
            }
            if overlay.run_id.is_some() {
                merged.run_id = overlay.run_id.clone();
            }
            if overlay.stage_id.is_some() {
                merged.stage_id = overlay.stage_id.clone();
            }
            if overlay.worker_id.is_some() {
                merged.worker_id = overlay.worker_id.clone();
            }
        }
        merged
    })
}

pub fn register_runtime_context_builtins(vm: &mut crate::vm::Vm) {
    for name in [
        "runtime_context",
        "task_current",
        "runtime_context_values",
        "runtime_context_get",
        "runtime_context_set",
        "runtime_context_clear",
    ] {
        vm.register_builtin(name, move |_args, _out| {
            Err(VmError::Runtime(format!(
                "{name}: internal runtime context builtin was not intercepted"
            )))
        });
    }
}

pub(crate) fn dispatch_runtime_context_builtin(
    vm: &mut crate::vm::Vm,
    name: &str,
    args: &[VmValue],
) -> Option<Result<VmValue, VmError>> {
    match name {
        "runtime_context" | "task_current" => Some(Ok(runtime_context_value(vm))),
        "runtime_context_values" => Some(Ok(VmValue::Dict(Rc::new(
            vm.runtime_context.values.clone(),
        )))),
        "runtime_context_get" => Some(runtime_context_get(vm, args)),
        "runtime_context_set" => Some(runtime_context_set(vm, args)),
        "runtime_context_clear" => Some(runtime_context_clear(vm, args)),
        _ => None,
    }
}

fn runtime_context_get(vm: &crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "runtime_context_get")?;
    Ok(vm
        .runtime_context
        .values
        .get(&key)
        .cloned()
        .or_else(|| args.get(1).cloned())
        .unwrap_or(VmValue::Nil))
}

fn runtime_context_set(vm: &mut crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "runtime_context_set")?;
    let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    Ok(vm
        .runtime_context
        .values
        .insert(key, value)
        .unwrap_or(VmValue::Nil))
}

fn runtime_context_clear(vm: &mut crate::vm::Vm, args: &[VmValue]) -> Result<VmValue, VmError> {
    let key = require_key(args, "runtime_context_clear")?;
    Ok(vm
        .runtime_context
        .values
        .remove(&key)
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

pub(crate) fn runtime_context_value(vm: &crate::vm::Vm) -> VmValue {
    let overlay = current_overlay();
    let mutation = crate::orchestration::current_mutation_session();
    let dispatch = crate::triggers::dispatcher::current_dispatch_context();
    let trace_context = crate::stdlib::tracing::current_trace_context();
    let agent_session_id = crate::agent_sessions::current_session_id();
    let agent_ancestry = agent_session_id
        .as_deref()
        .and_then(crate::agent_sessions::ancestry);
    let cancelled = vm
        .cancel_token
        .as_ref()
        .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst));

    let workflow_id = overlay.workflow_id;
    let run_id = overlay
        .run_id
        .or_else(|| mutation.as_ref().and_then(|session| session.run_id.clone()));
    let stage_id = overlay.stage_id;
    let worker_id = overlay.worker_id.or_else(|| {
        mutation
            .as_ref()
            .and_then(|session| session.worker_id.clone())
    });

    let mut values = BTreeMap::new();
    insert_string(
        &mut values,
        "task_id",
        Some(vm.runtime_context.task_id.clone()),
    );
    insert_string(
        &mut values,
        "parent_task_id",
        vm.runtime_context.parent_task_id.clone(),
    );
    insert_string(
        &mut values,
        "root_task_id",
        Some(vm.runtime_context.root_task_id.clone()),
    );
    insert_string(
        &mut values,
        "task_name",
        vm.runtime_context.task_name.clone(),
    );
    insert_string(
        &mut values,
        "task_group_id",
        vm.runtime_context.task_group_id.clone(),
    );
    insert_string(&mut values, "scope_id", vm.runtime_context.scope_id.clone());
    insert_string(&mut values, "workflow_id", workflow_id);
    insert_string(&mut values, "run_id", run_id);
    insert_string(&mut values, "stage_id", stage_id);
    insert_string(&mut values, "worker_id", worker_id);
    insert_string(&mut values, "agent_session_id", agent_session_id.clone());
    insert_string(
        &mut values,
        "parent_agent_session_id",
        agent_ancestry
            .as_ref()
            .and_then(|ancestry| ancestry.parent_id.clone()),
    );
    insert_string(
        &mut values,
        "root_agent_session_id",
        agent_ancestry
            .as_ref()
            .map(|ancestry| ancestry.root_id.clone()),
    );
    insert_string(&mut values, "agent_name", None);

    if let Some(context) = dispatch {
        insert_string(&mut values, "trigger_id", Some(context.binding_id.clone()));
        insert_string(
            &mut values,
            "trigger_event_id",
            Some(context.trigger_event.id.0.clone()),
        );
        insert_string(
            &mut values,
            "binding_key",
            Some(format!(
                "{}@{}",
                context.binding_id, context.binding_version
            )),
        );
        insert_string(
            &mut values,
            "tenant_id",
            context.trigger_event.tenant_id.map(|tenant| tenant.0),
        );
        insert_string(
            &mut values,
            "provider",
            Some(context.trigger_event.provider.0),
        );
        insert_string(
            &mut values,
            "trace_id",
            Some(context.trigger_event.trace_id.0),
        );
    } else {
        insert_string(&mut values, "trigger_id", None);
        insert_string(&mut values, "trigger_event_id", None);
        insert_string(&mut values, "binding_key", None);
        insert_string(&mut values, "tenant_id", None);
        insert_string(&mut values, "provider", None);
        insert_string(
            &mut values,
            "trace_id",
            trace_context.as_ref().map(|context| context.0.clone()),
        );
    }

    insert_string(
        &mut values,
        "span_id",
        trace_context
            .as_ref()
            .map(|context| context.1.clone())
            .or_else(|| crate::tracing::current_span_id().map(|id| id.to_string())),
    );
    insert_string(&mut values, "scheduler_key", None);
    insert_string(&mut values, "runner", None);
    insert_string(&mut values, "capacity_class", None);
    values.insert(
        "context_values".to_string(),
        VmValue::Dict(Rc::new(vm.runtime_context.values.clone())),
    );
    values.insert("cancelled".to_string(), VmValue::Bool(cancelled));
    values.insert("debug".to_string(), debug_context_value(vm, cancelled));
    VmValue::Dict(Rc::new(values))
}

fn debug_context_value(vm: &crate::vm::Vm, cancelled: bool) -> VmValue {
    let mut debug = BTreeMap::new();
    debug.insert("cancelled".to_string(), VmValue::Bool(cancelled));
    debug.insert("waiting_reason".to_string(), VmValue::Nil);
    debug.insert(
        "active_task_ids".to_string(),
        VmValue::List(Rc::new(
            vm.spawned_tasks
                .keys()
                .map(|id| VmValue::String(Rc::from(id.as_str())))
                .collect(),
        )),
    );
    debug.insert(
        "held_synchronization".to_string(),
        VmValue::List(Rc::new(Vec::new())),
    );
    VmValue::Dict(Rc::new(debug))
}

fn insert_string(values: &mut BTreeMap<String, VmValue>, key: &str, value: Option<String>) {
    values.insert(
        key.to_string(),
        value
            .map(|value| VmValue::String(Rc::from(value)))
            .unwrap_or(VmValue::Nil),
    );
}
