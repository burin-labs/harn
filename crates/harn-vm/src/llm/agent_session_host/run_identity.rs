use crate::value::{VmDictExt, VmValue};

/// Exact run identity owned by the active Harn-driven agent-loop invocation.
pub(crate) fn active_run_id(session_id: &str) -> Option<String> {
    super::AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(session_id)
            .map(|session| session.run_id.clone())
    })
}

pub(super) fn agent_init_control_done(
    session_id: &str,
    run_id: &str,
    task: &str,
    system: Option<&str>,
    mut result: VmValue,
) -> VmValue {
    if let VmValue::Dict(map) = &mut result {
        let mut values = (**map).clone();
        values.put_str("run_id", run_id);
        *map = std::sync::Arc::new(values);
    }
    agent_init_control(session_id, run_id, task, system, 0, 0, true, Some(result))
}

pub(super) fn agent_init_control(
    session_id: &str,
    run_id: &str,
    task: &str,
    system: Option<&str>,
    max_iterations: i64,
    max_verify_attempts: i64,
    done: bool,
    result: Option<VmValue>,
) -> VmValue {
    let mut control = crate::value::DictMap::new();
    control.put_str("session_id", session_id);
    control.put_str("run_id", run_id);
    control.put_str("task", task);
    control.insert(
        crate::value::intern_key("system"),
        system
            .map(|s| VmValue::String(arcstr::ArcStr::from(s.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    control.insert(
        crate::value::intern_key("max_iterations"),
        VmValue::Int(max_iterations),
    );
    control.insert(
        crate::value::intern_key("max_verify_attempts"),
        VmValue::Int(max_verify_attempts),
    );
    control.insert(crate::value::intern_key("done"), VmValue::Bool(done));
    if let Some(result) = result {
        control.insert(crate::value::intern_key("result"), result);
    }
    VmValue::dict(control)
}
