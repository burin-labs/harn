//! Tool, persona, and step hook registration builtins for workflow execution.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

pub(super) type PostHookFn = Rc<dyn Fn(&str, &str) -> crate::orchestration::PostToolAction>;

pub(super) fn register_tool_hook_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .and_then(|a| a.as_dict())
        .cloned()
        .unwrap_or_default();
    let pattern = config
        .get("pattern")
        .map(|v| v.display())
        .unwrap_or_else(|| "*".to_string());
    let deny_reason = config.get("deny").map(|v| v.display());
    let max_output = config.get("max_output").and_then(|v| match v {
        VmValue::Int(n) => Some(*n as usize),
        _ => None,
    });

    let pre: Option<crate::orchestration::PreToolHookFn> = deny_reason.map(|reason| {
        Rc::new(move |_name: &str, _args: &serde_json::Value| {
            crate::orchestration::PreToolAction::Deny(reason.clone())
        }) as _
    });

    let post: Option<PostHookFn> = max_output.map(|max| {
        Rc::new(move |_name: &str, result: &str| {
            if result.len() > max {
                crate::orchestration::PostToolAction::Modify(
                    crate::orchestration::microcompact_tool_output(result, max),
                )
            } else {
                crate::orchestration::PostToolAction::Pass
            }
        }) as _
    });

    crate::orchestration::register_tool_hook(crate::orchestration::ToolHook { pattern, pre, post });
    Ok(VmValue::Nil)
}

pub(super) fn clear_tool_hooks_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    crate::orchestration::clear_tool_hooks();
    Ok(VmValue::Nil)
}

pub(super) fn parse_persona_hook_event(
    value: &VmValue,
    builtin: &str,
) -> Result<(crate::orchestration::HookEvent, Option<f64>), VmError> {
    let raw = value.display();
    let event = raw.trim();
    if let Some(pct) = event
        .strip_prefix("OnBudgetThreshold(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let pct = pct.trim().parse::<f64>().map_err(|_| {
            VmError::Runtime(format!("{builtin}: invalid budget threshold `{pct}`"))
        })?;
        return Ok((
            crate::orchestration::HookEvent::OnBudgetThreshold,
            Some(pct),
        ));
    }
    let event = match event {
        "PreStep" => crate::orchestration::HookEvent::PreStep,
        "PostStep" => crate::orchestration::HookEvent::PostStep,
        "OnBudgetThreshold" => crate::orchestration::HookEvent::OnBudgetThreshold,
        "OnApprovalRequested" => crate::orchestration::HookEvent::OnApprovalRequested,
        "OnHandoffEmitted" => crate::orchestration::HookEvent::OnHandoffEmitted,
        "OnPersonaPaused" => crate::orchestration::HookEvent::OnPersonaPaused,
        "OnPersonaResumed" => crate::orchestration::HookEvent::OnPersonaResumed,
        other => {
            return Err(VmError::Runtime(format!(
                "{builtin}: unknown persona hook event `{other}`"
            )))
        }
    };
    Ok((event, None))
}

pub(super) fn required_hook_closure(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<Rc<crate::value::VmClosure>, VmError> {
    match args.get(index) {
        Some(VmValue::Closure(closure)) => Ok(closure.clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: handler must be a closure, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: missing handler"))),
    }
}

pub(super) fn register_persona_hook_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let persona_pattern = args
        .first()
        .map(VmValue::display)
        .unwrap_or_else(|| "*".to_string());
    let (event, threshold_pct) = parse_persona_hook_event(
        args.get(1)
            .ok_or_else(|| VmError::Runtime("register_persona_hook: missing event".to_string()))?,
        "register_persona_hook",
    )?;
    let handler = required_hook_closure(args, 2, "register_persona_hook")?;
    crate::step_runtime::register_persona_hook(persona_pattern, event, threshold_pct, handler);
    Ok(VmValue::Nil)
}

pub(super) fn register_step_hook_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let persona_pattern = args
        .first()
        .map(VmValue::display)
        .unwrap_or_else(|| "*".to_string());
    let step_name = args
        .get(1)
        .map(VmValue::display)
        .ok_or_else(|| VmError::Runtime("register_step_hook: missing step name".to_string()))?;
    let (event, threshold_pct) = parse_persona_hook_event(
        args.get(2)
            .ok_or_else(|| VmError::Runtime("register_step_hook: missing event".to_string()))?,
        "register_step_hook",
    )?;
    let handler = required_hook_closure(args, 3, "register_step_hook")?;
    crate::step_runtime::register_step_hook(
        persona_pattern,
        step_name,
        event,
        threshold_pct,
        handler,
    );
    Ok(VmValue::Nil)
}

pub(super) fn clear_persona_hooks_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    crate::step_runtime::clear_persona_hooks();
    Ok(VmValue::Nil)
}

pub(super) fn register_session_hook_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let (event_arg, pattern, handler_arg) = match args.len() {
        2 => (&args[0], "*".to_string(), &args[1]),
        3 => (&args[0], args[1].display(), &args[2]),
        n => {
            return Err(VmError::Runtime(format!(
                "register_session_hook expects 2 or 3 arguments (event, pattern?, handler); got {n}"
            )));
        }
    };
    let event_name = event_arg.display();
    let event = crate::orchestration::HookEvent::parse_session_event(&event_name)
        .map_err(|message| VmError::Runtime(format!("register_session_hook: {message}")))?;
    let VmValue::Closure(closure) = handler_arg else {
        return Err(VmError::Runtime(format!(
            "register_session_hook: handler must be a closure, got {}",
            handler_arg.type_name()
        )));
    };
    let handler_name = format!("session_hook::{}", event.as_str());
    crate::orchestration::register_vm_hook(event, pattern, handler_name, closure.clone());
    Ok(VmValue::Nil)
}

pub(super) fn clear_session_hooks_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    crate::orchestration::clear_session_hooks();
    Ok(VmValue::Nil)
}

/// Register the callback `Vm::execute` invokes after the pipeline's
/// declared steps complete (signature `fn(harness, return_value)`). Last-
/// write-wins; the callback's return value replaces the pipeline's return
/// value when the lifecycle runs.
pub(super) fn pipeline_on_finish_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handler = required_hook_closure(args, 0, "pipeline_on_finish")?;
    crate::orchestration::set_pipeline_on_finish(handler);
    Ok(VmValue::Nil)
}

/// Fire a session-level lifecycle hook from Harn. Used by the
/// Harn-driven agent loop (autocompact, file edits, etc.) to invoke
/// hooks that are wired in Harn rather than from a Rust host primitive.
///
/// Returns a dict shaped like:
///   { control: "allow" | "block" | "decision", reason?, decision? }
pub(super) async fn fire_session_hook_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let event_name = args
        .first()
        .map(VmValue::display)
        .ok_or_else(|| VmError::Runtime("__host_fire_session_hook: missing event".to_string()))?;
    let event = crate::orchestration::HookEvent::parse_session_event(&event_name)
        .map_err(|message| VmError::Runtime(format!("__host_fire_session_hook: {message}")))?;
    let payload_value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let mut payload = crate::llm::vm_value_to_json(&payload_value);
    if !payload.is_object() {
        payload = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut payload {
        map.entry("event".to_string())
            .or_insert_with(|| serde_json::Value::String(event.as_str().to_string()));
    }

    let control = crate::orchestration::run_lifecycle_hooks_with_control(event, &payload).await?;
    crate::run_events::emit(crate::run_events::RunEvent::Hook {
        name: event_name.clone(),
        phase: match &control {
            crate::orchestration::HookControl::Allow => "allow".to_string(),
            crate::orchestration::HookControl::Block { .. } => "block".to_string(),
            crate::orchestration::HookControl::Decision { kind, .. } => format!("decision:{kind}"),
        },
        payload: payload.clone(),
    });
    let mut out: BTreeMap<String, VmValue> = BTreeMap::new();
    match control {
        crate::orchestration::HookControl::Allow => {
            out.insert("control".to_string(), VmValue::String(Rc::from("allow")));
        }
        crate::orchestration::HookControl::Block { reason } => {
            out.insert("control".to_string(), VmValue::String(Rc::from("block")));
            out.insert("reason".to_string(), VmValue::String(Rc::from(reason)));
        }
        crate::orchestration::HookControl::Decision { kind, reason } => {
            out.insert("control".to_string(), VmValue::String(Rc::from("decision")));
            out.insert("decision".to_string(), VmValue::String(Rc::from(kind)));
            if let Some(reason) = reason {
                out.insert("reason".to_string(), VmValue::String(Rc::from(reason)));
            }
        }
    }
    Ok(VmValue::Dict(Rc::new(out)))
}

/// Synchronous emit-only entry point: explicitly notify the session
/// that a file was edited. Records a `file_edited` advisory event on
/// the active transcript so replay tooling can see it, and queues VM
/// closure handlers for the next async-builtin boundary.
pub(super) fn notify_file_edited_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(VmValue::display)
        .ok_or_else(|| VmError::Runtime("notify_file_edited: missing path".to_string()))?;
    let metadata = args
        .get(1)
        .map(crate::llm::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    crate::orchestration::queue_file_edited(&path, metadata);
    Ok(VmValue::Nil)
}

/// Drain the file-edit queue and fire `FileEdited` hooks for any
/// notifications recorded since the last drain. Returns the list of
/// drained paths so callers (the agent loop) can record them on the
/// transcript or pass them to follow-up tools.
pub(super) async fn drain_file_edits_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = args.first().map(VmValue::display).unwrap_or_default();
    let drained = crate::orchestration::drain_file_edits();
    let mut paths: Vec<VmValue> = Vec::with_capacity(drained.len());
    for edit in drained {
        let payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::FileEdited.as_str(),
            "session": {"id": &session_id},
            "path": edit.path,
            "metadata": edit.metadata,
        });
        crate::orchestration::run_lifecycle_hooks(
            crate::orchestration::HookEvent::FileEdited,
            &payload,
        )
        .await?;
        paths.push(VmValue::String(Rc::from(edit.path)));
    }
    Ok(VmValue::List(Rc::new(paths)))
}
