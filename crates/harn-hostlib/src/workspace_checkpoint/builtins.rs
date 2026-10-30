//! Argument decoding for the checkpoint capability's registered builtins.

use std::path::PathBuf;

use harn_vm::tool_annotations::SideEffectLevel;
use harn_vm::VmValue;
use serde::Serialize;

use super::{
    call_begin, call_end, capture_with_writer_policy, drop_session, plan,
    restore_with_confirmations, ConfirmedPath, WriterPolicy, CALL_BEGIN_BUILTIN, CALL_END_BUILTIN,
    CAPTURE_BUILTIN, DROP_BUILTIN, PLAN_BUILTIN, RESTORE_BUILTIN,
};
use crate::error::HostlibError;
use crate::tools::args::{dict_arg, optional_string, require_string};

fn to_vm<T: Serialize>(value: &T) -> VmValue {
    harn_vm::json_to_vm_value(&serde_json::to_value(value).expect("checkpoint result serializes"))
}

pub(super) fn capture_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(CAPTURE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(CAPTURE_BUILTIN, dict, "session_id")?;
    let boundary_id = require_string(CAPTURE_BUILTIN, dict, "boundary_id")?;
    let root = optional_string(CAPTURE_BUILTIN, dict, "root")?.map(PathBuf::from);
    let policy = optional_string(CAPTURE_BUILTIN, dict, "writer_policy")?
        .map(|raw| {
            serde_json::from_value::<WriterPolicy>(serde_json::Value::String(raw.clone())).map_err(
                |_| HostlibError::InvalidParameter {
                    builtin: CAPTURE_BUILTIN,
                    param: "writer_policy",
                    message: format!("unknown writer policy `{raw}`"),
                },
            )
        })
        .transpose()?;
    Ok(to_vm(&capture_with_writer_policy(
        &session_id,
        &boundary_id,
        root.as_deref(),
        policy,
    )?))
}

pub(super) fn call_begin_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(CALL_BEGIN_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(CALL_BEGIN_BUILTIN, dict, "session_id")?;
    let call_id = require_string(CALL_BEGIN_BUILTIN, dict, "call_id")?;
    Ok(to_vm(&call_begin(&session_id, &call_id)?))
}

pub(super) fn call_end_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(CALL_END_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(CALL_END_BUILTIN, dict, "session_id")?;
    let call_id = require_string(CALL_END_BUILTIN, dict, "call_id")?;
    let side_effect = optional_string(CALL_END_BUILTIN, dict, "side_effect")?
        .map(|raw| {
            serde_json::from_value::<SideEffectLevel>(serde_json::Value::String(raw.clone()))
                .map_err(|_| HostlibError::InvalidParameter {
                    builtin: CALL_END_BUILTIN,
                    param: "side_effect",
                    message: format!("unknown side-effect class `{raw}`"),
                })
        })
        .transpose()?;
    Ok(to_vm(&call_end(&session_id, &call_id, side_effect)?))
}

pub(super) fn plan_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(PLAN_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(PLAN_BUILTIN, dict, "session_id")?;
    let boundary_id = require_string(PLAN_BUILTIN, dict, "boundary_id")?;
    Ok(to_vm(&plan(&session_id, &boundary_id)?))
}

pub(super) fn restore_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(RESTORE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(RESTORE_BUILTIN, dict, "session_id")?;
    let boundary_id = require_string(RESTORE_BUILTIN, dict, "boundary_id")?;
    let paths = crate::value_args::optional_string_list(RESTORE_BUILTIN, dict, "paths")?;
    let confirmed_paths = match dict.get("confirmed_paths") {
        None | Some(VmValue::Nil) => Vec::new(),
        Some(value) => {
            let value = harn_vm::llm::vm_value_to_json_strict(value, "confirmed_paths").map_err(
                |message| HostlibError::InvalidParameter {
                    builtin: RESTORE_BUILTIN,
                    param: "confirmed_paths",
                    message,
                },
            )?;
            serde_json::from_value::<Vec<ConfirmedPath>>(value).map_err(|error| {
                HostlibError::InvalidParameter {
                    builtin: RESTORE_BUILTIN,
                    param: "confirmed_paths",
                    message: error.to_string(),
                }
            })?
        }
    };
    Ok(to_vm(&restore_with_confirmations(
        &session_id,
        &boundary_id,
        paths.as_deref(),
        &confirmed_paths,
    )?))
}

pub(super) fn drop_builtin(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(DROP_BUILTIN, args)?;
    let dict = raw.as_ref();
    let session_id = require_string(DROP_BUILTIN, dict, "session_id")?;
    Ok(to_vm(&drop_session(&session_id)))
}
