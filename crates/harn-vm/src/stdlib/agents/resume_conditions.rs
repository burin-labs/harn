use harn_parser::diagnostic_codes::Code;

use super::diagnostic_error;
use crate::value::{VmError, VmValue};

pub(super) fn resume_conditions_error(field: &str, message: impl Into<String>) -> VmError {
    diagnostic_error(
        Code::ResumeConditionsInvalid,
        format!("invalid ResumeConditions.{field}: {}", message.into()),
    )
}

pub(super) fn parse_resume_trigger_condition(
    value: &VmValue,
) -> Result<Option<serde_json::Value>, VmError> {
    match value {
        VmValue::Nil => Ok(None),
        VmValue::Dict(map) => {
            crate::stdlib::triggers_stdlib::validate_resume_trigger_spec(map)
                .map_err(|error| resume_conditions_error("trigger", error.to_string()))?;
            Ok(Some(crate::llm::vm_value_to_json(value)))
        }
        other => Err(resume_conditions_error(
            "trigger",
            format!("expected dict or nil, got {}", other.type_name()),
        )),
    }
}

pub(super) fn parse_resume_timeout_condition(
    value: &VmValue,
) -> Result<Option<serde_json::Value>, VmError> {
    let VmValue::Dict(map) = value else {
        if matches!(value, VmValue::Nil) {
            return Ok(None);
        }
        return Err(resume_conditions_error(
            "timeout",
            format!("expected dict or nil, got {}", value.type_name()),
        ));
    };
    for key in map.keys() {
        if key != "duration_minutes" && key != "on_timeout" {
            return Err(resume_conditions_error(
                &format!("timeout.{key}"),
                "unknown field; expected duration_minutes or on_timeout",
            ));
        }
    }
    let duration = map
        .get("duration_minutes")
        .and_then(VmValue::as_int)
        .ok_or_else(|| {
            resume_conditions_error("timeout.duration_minutes", "must be a positive int")
        })?;
    if duration <= 0 {
        return Err(resume_conditions_error(
            "timeout.duration_minutes",
            "must be a positive int",
        ));
    }
    let on_timeout = match map.get("on_timeout") {
        Some(VmValue::Nil) | None => "resume_with_summary".to_string(),
        Some(VmValue::String(action))
            if matches!(
                action.as_ref(),
                "resume_with_summary" | "fail" | "resume_with_input"
            ) =>
        {
            action.to_string()
        }
        Some(VmValue::String(action)) => {
            return Err(resume_conditions_error(
                "timeout.on_timeout",
                format!(
                    "unsupported action `{action}`, expected resume_with_summary|fail|resume_with_input"
                ),
            ))
        }
        Some(other) => {
            return Err(resume_conditions_error(
                "timeout.on_timeout",
                format!("expected string, got {}", other.type_name()),
            ))
        }
    };
    Ok(Some(serde_json::json!({
        "duration_minutes": duration,
        "on_timeout": on_timeout,
    })))
}

pub(super) fn parse_resume_event_condition(
    value: &VmValue,
) -> Result<Option<serde_json::Value>, VmError> {
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(text) if !text.trim().is_empty() => {
            let trimmed = text.trim();
            crate::event_log::Topic::new(trimmed.to_string()).map_err(|error| {
                resume_conditions_error(
                    "on_event",
                    format!("invalid runtime event channel: {error}"),
                )
            })?;
            Ok(Some(serde_json::json!(trimmed.to_string())))
        }
        VmValue::String(_) => Err(resume_conditions_error(
            "on_event",
            "must be a non-empty string",
        )),
        other => Err(resume_conditions_error(
            "on_event",
            format!("expected string or nil, got {}", other.type_name()),
        )),
    }
}

pub(super) fn parse_resume_conditions_value(value: Option<&VmValue>) -> Result<VmValue, VmError> {
    let Some(value) = value else {
        return Ok(VmValue::Nil);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(VmValue::Nil);
    }
    let VmValue::Dict(map) = value else {
        return Err(resume_conditions_error(
            "root",
            format!("expected dict or nil, got {}", value.type_name()),
        ));
    };
    let valid_keys = ["trigger", "timeout", "on_event"];
    for key in map.keys() {
        if !valid_keys.contains(&key.as_str()) {
            return Err(resume_conditions_error(
                key,
                "unknown field; expected trigger, timeout, or on_event",
            ));
        }
    }

    let mut normalized = serde_json::Map::new();
    if let Some(trigger) = map
        .get("trigger")
        .map(parse_resume_trigger_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("trigger".to_string(), trigger);
    }
    if let Some(timeout) = map
        .get("timeout")
        .map(parse_resume_timeout_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("timeout".to_string(), timeout);
    }
    if let Some(event) = map
        .get("on_event")
        .map(parse_resume_event_condition)
        .transpose()?
        .flatten()
    {
        normalized.insert("on_event".to_string(), event);
    }
    Ok(crate::stdlib::json_to_vm_value(&serde_json::Value::Object(
        normalized,
    )))
}
