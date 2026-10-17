use crate::value::VmDictExt;
use std::cell::RefCell;

use crate::value::VmValue;

pub use crate::vm::RuntimeContext;
pub(crate) use crate::vm::{runtime_context_clear, runtime_context_get, runtime_context_set};

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

/// Per-task ambient-scope swap of the runtime-context overlay stack. See
/// `orchestration::ambient_scope`: the overlay attributes events (worker_id,
/// run_id) to the running task, so it must follow that task across `.await`
/// rather than leak to cooperatively-scheduled siblings.
pub(crate) fn swap_runtime_context_overlay_stack(
    next: Vec<RuntimeContextOverlay>,
) -> Vec<RuntimeContextOverlay> {
    RUNTIME_CONTEXT_OVERLAY_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
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

/// Canonical session/run association for the active agent invocation.
pub(crate) fn current_agent_run_ref() -> Option<crate::agent_events::AgentRunRef> {
    let session_id = crate::agent_sessions::current_session_id()?;
    let run_id = crate::llm::active_agent_run_id(&session_id)
        .or_else(|| crate::agent_sessions::active_run_id(&session_id))?;
    Some(crate::agent_events::AgentRunRef { session_id, run_id })
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
    let cancelled = vm.is_cancel_requested();

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

    let mut values = crate::value::DictMap::new();
    insert_string(
        &mut values,
        "task_id",
        Some(vm.runtime_context().task_id.clone()),
    );
    insert_string(
        &mut values,
        "parent_task_id",
        vm.runtime_context().parent_task_id.clone(),
    );
    insert_string(
        &mut values,
        "root_task_id",
        Some(vm.runtime_context().root_task_id.clone()),
    );
    insert_string(
        &mut values,
        "task_name",
        vm.runtime_context().task_name.clone(),
    );
    insert_string(
        &mut values,
        "task_group_id",
        vm.runtime_context().task_group_id.clone(),
    );
    insert_string(
        &mut values,
        "scope_id",
        vm.runtime_context().scope_id.clone(),
    );
    insert_string(&mut values, "workflow_id", workflow_id);
    insert_string(&mut values, "run_id", run_id);
    insert_string(&mut values, "stage_id", stage_id);
    insert_string(&mut values, "worker_id", worker_id);
    insert_string(&mut values, "agent_session_id", agent_session_id);
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
        // Outside a trigger dispatch, fall back to the ambient
        // `enter_tenant` scope hosts install (today: harn-serve binds
        // it from the authenticated principal). Keeps
        // `runtime_context.tenant_id` consistent across trigger and
        // non-trigger code paths.
        insert_string(
            &mut values,
            "tenant_id",
            crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0),
        );
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
    values.put(
        "context_values",
        VmValue::dict(vm.runtime_context().values.clone()),
    );
    values.put_bool("cancelled", cancelled);
    values.put("debug", debug_context_value(vm, cancelled));
    VmValue::dict(values)
}

fn debug_context_value(vm: &crate::vm::Vm, cancelled: bool) -> VmValue {
    let mut debug = crate::value::DictMap::new();
    debug.put_bool("cancelled", cancelled);
    debug.put("waiting_reason", VmValue::Nil);
    debug.put(
        "active_task_ids",
        VmValue::list(
            vm.active_task_ids()
                .map(|id| VmValue::String(arcstr::ArcStr::from(id)))
                .collect(),
        ),
    );
    debug.put("held_synchronization", VmValue::list(Vec::new()));
    debug.put(
        "supervisors",
        crate::stdlib::supervisor::supervisor_debug_values(),
    );
    VmValue::dict(debug)
}

fn insert_string(values: &mut crate::value::DictMap, key: &str, value: Option<String>) {
    values.insert(
        crate::value::intern_key(key),
        value.map(VmValue::string).unwrap_or(VmValue::Nil),
    );
}
