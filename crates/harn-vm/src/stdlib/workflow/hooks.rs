//! Tool, persona, and step hook registration builtins for workflow execution.

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
