use crate::value::{VmError, VmValue};

use super::err;

fn dict_string_field(dict: &crate::value::DictMap, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn close_status_arg(
    args: &[VmValue],
) -> Result<(String, String, serde_json::Value), VmError> {
    match args.get(1) {
        None | Some(VmValue::Nil) => Ok((
            "closed".to_string(),
            "closed".to_string(),
            serde_json::Value::Null,
        )),
        Some(VmValue::String(value)) => {
            let reason = value.trim();
            if reason.is_empty() {
                return Err(err(
                    "agent_session_close: `status` string must not be empty",
                ));
            }
            Ok((
                reason.to_string(),
                reason.to_string(),
                serde_json::Value::Null,
            ))
        }
        Some(VmValue::Dict(dict)) => {
            let reason = dict_string_field(dict, "reason")
                .or_else(|| dict_string_field(dict, "stop_reason"))
                .or_else(|| dict_string_field(dict, "status"))
                .unwrap_or_else(|| "closed".to_string());
            let status = dict_string_field(dict, "status").unwrap_or_else(|| reason.clone());
            Ok((
                reason,
                status,
                crate::llm::helpers::vm_value_to_json(args.get(1).expect("status arg")),
            ))
        }
        _ => Err(err(
            "agent_session_close: `status` must be a string, dict, or nil",
        )),
    }
}
