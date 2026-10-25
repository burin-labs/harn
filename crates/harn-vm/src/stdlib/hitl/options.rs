use serde_json::Value as JsonValue;
use std::time::Duration as StdDuration;

use super::{HITL_APPROVAL_TIMEOUT_MS, HITL_QUESTION_TIMEOUT_MS};
use crate::stdlib::args::{ArgError, Args, ErrorKind};
use crate::value::{VmError, VmValue};

#[derive(Clone, Debug)]
pub(super) struct AskUserOptions {
    pub(super) schema: Option<VmValue>,
    pub(super) timeout: Option<StdDuration>,
    pub(super) default: Option<VmValue>,
}

#[derive(Clone, Debug)]
pub(super) struct ApprovalOptions {
    pub(super) detail: Option<VmValue>,
    pub(super) args: Option<VmValue>,
    pub(super) quorum: u32,
    pub(super) reviewers: Vec<String>,
    pub(super) deadline: StdDuration,
    pub(super) principal: Option<String>,
    pub(super) evidence_refs: Vec<JsonValue>,
    pub(super) undo_metadata: Option<JsonValue>,
    pub(super) capabilities_requested: Vec<String>,
}

pub(super) fn parse_ask_user_options(value: Option<&VmValue>) -> Result<AskUserOptions, VmError> {
    let Some(value) = value else {
        return Ok(AskUserOptions {
            schema: None,
            timeout: Some(default_question_timeout()),
            default: None,
        });
    };
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime("ask_user: options must be a dict".to_string()))?;
    Ok(AskUserOptions {
        schema: dict
            .get("schema")
            .cloned()
            .filter(|value| !matches!(value, VmValue::Nil)),
        timeout: dict
            .get("timeout")
            .map(parse_duration_value)
            .transpose()?
            .or_else(|| Some(default_question_timeout())),
        default: dict
            .get("default")
            .cloned()
            .filter(|value| !matches!(value, VmValue::Nil)),
    })
}

fn default_question_timeout() -> StdDuration {
    StdDuration::from_millis(HITL_QUESTION_TIMEOUT_MS)
}

pub(super) fn parse_approval_options(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<ApprovalOptions, VmError> {
    let dict = match value {
        None => None,
        Some(VmValue::Dict(dict)) => Some(dict),
        Some(_) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: options must be a dict"
            )))
        }
    };
    let quorum = dict
        .and_then(|dict| dict.get("quorum"))
        .and_then(VmValue::as_int)
        .unwrap_or(1);
    if quorum <= 0 {
        return Err(VmError::Runtime(format!(
            "{builtin}: quorum must be positive"
        )));
    }
    let reviewers = optional_string_list(dict.and_then(|dict| dict.get("reviewers")), builtin)?;
    let capabilities_requested = optional_string_list(
        dict.and_then(|dict| dict.get("capabilities_requested")),
        builtin,
    )?;
    let evidence_refs = dict
        .and_then(|dict| dict.get("evidence_refs"))
        .map(|value| match value {
            VmValue::List(items) => Ok(items
                .iter()
                .map(crate::llm::vm_value_to_json)
                .collect::<Vec<_>>()),
            _ => Err(VmError::Runtime(format!(
                "{builtin}: evidence_refs must be a list"
            ))),
        })
        .transpose()?
        .unwrap_or_default();
    let deadline = dict
        .and_then(|dict| dict.get("deadline"))
        .map(parse_duration_value)
        .transpose()?
        .unwrap_or_else(|| StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS));
    Ok(ApprovalOptions {
        detail: dict.and_then(|dict| dict.get("detail")).cloned(),
        args: dict.and_then(|dict| dict.get("args")).cloned(),
        quorum: quorum as u32,
        reviewers,
        deadline,
        principal: dict
            .and_then(|dict| dict.get("principal"))
            .map(VmValue::display)
            .filter(|value| !value.is_empty()),
        evidence_refs,
        undo_metadata: dict
            .and_then(|dict| dict.get("undo_metadata"))
            .map(crate::llm::vm_value_to_json),
        capabilities_requested,
    })
}

/// A quorum bound must be at least one.
pub(super) fn quorum_arg(args: &Args<'_, '_>, idx: usize, name: &str) -> Result<i64, VmError> {
    match args.int(idx, name)? {
        value if value > 0 => Ok(value),
        _ => Err(ArgError::constraint(
            args.fn_name(),
            args.kind(),
            name,
            "must be > 0",
        )),
    }
}

pub(super) fn optional_string_list(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::List(list) => Ok(list.iter().map(VmValue::display).collect()),
        _ => Err(VmError::Runtime(format!(
            "{builtin}: expected list<string>"
        ))),
    }
}

fn parse_duration_value(value: &VmValue) -> Result<StdDuration, VmError> {
    Args::single("hitl", ErrorKind::Runtime, Some(value)).duration(0, "timeout")
}

pub(super) fn coerce_like_default(value: &VmValue, default: &VmValue) -> VmValue {
    match default {
        VmValue::Int(_) => match value {
            VmValue::Int(_) => value.clone(),
            VmValue::Float(number) => VmValue::Int(*number as i64),
            VmValue::String(text) => text
                .parse::<i64>()
                .map(VmValue::Int)
                .unwrap_or_else(|_| default.clone()),
            _ => default.clone(),
        },
        VmValue::Float(_) => match value {
            VmValue::Float(_) => value.clone(),
            VmValue::Int(number) => VmValue::Float(*number as f64),
            VmValue::String(text) => text
                .parse::<f64>()
                .map(VmValue::Float)
                .unwrap_or_else(|_| default.clone()),
            _ => default.clone(),
        },
        VmValue::Bool(_) => match value {
            VmValue::Bool(_) => value.clone(),
            VmValue::String(text) if text.eq_ignore_ascii_case("true") => VmValue::Bool(true),
            VmValue::String(text) if text.eq_ignore_ascii_case("false") => VmValue::Bool(false),
            _ => default.clone(),
        },
        VmValue::String(_) => VmValue::string(value.display()),
        VmValue::Duration(_) => match value {
            VmValue::Duration(_) => value.clone(),
            VmValue::Int(ms) => VmValue::Duration(*ms),
            _ => default.clone(),
        },
        VmValue::Nil => value.clone(),
        _ => {
            if value.type_name() == default.type_name() {
                value.clone()
            } else {
                default.clone()
            }
        }
    }
}

pub(super) fn vm_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

pub(super) fn vm_string(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(text) => Some(text.as_ref()),
        _ => None,
    }
}

pub(super) fn vm_string_list(value: &VmValue) -> Option<Vec<String>> {
    match value {
        VmValue::List(values) => Some(values.iter().map(VmValue::display).collect()),
        _ => None,
    }
}
