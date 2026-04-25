use std::collections::BTreeMap;
use std::rc::Rc;

use crate::bridge::json_result_to_vm_value;
use crate::connectors::{
    active_connector_client, harn_module::active_harn_connector_ctx, ClientError,
};
use crate::event_log::{EventLog, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::secrets::{SecretId, SecretVersion};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_connector_builtins(vm: &mut Vm) {
    vm.register_async_builtin("connector_call", |args| async move {
        let provider = required_string_arg(&args, 0, "connector_call", "provider")?;
        let method = required_string_arg(&args, 1, "connector_call", "method")?;
        let params = match args.get(2) {
            Some(VmValue::Dict(dict)) => vm_value_to_json(&VmValue::Dict(dict.clone())),
            Some(value) if !matches!(value, VmValue::Nil) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "connector_call: params must be a dict when provided",
                ))));
            }
            _ => vm_value_to_json(&VmValue::Dict(Rc::new(BTreeMap::new()))),
        };

        let client = active_connector_client(&provider).ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "connector_call: connector `{provider}` is not active"
            ))))
        })?;

        let result = client
            .call(&method, params)
            .await
            .map_err(client_error_to_vm)?;
        Ok(json_result_to_vm_value(&result))
    });

    vm.register_async_builtin("secret_get", |args| async move {
        let raw = required_string_arg(&args, 0, "secret_get", "secret_id")?;
        let ctx = active_harn_connector_ctx().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "secret_get: no active Harn connector context",
            )))
        })?;
        let secret_id = parse_secret_id(raw.as_str()).ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "secret_get: expected secret id in namespace/name or namespace/name@version form",
            )))
        })?;
        let secret = ctx.secrets.get(&secret_id).await.map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!("secret_get: {error}"))))
        })?;
        secret.with_exposed(|bytes| {
            std::str::from_utf8(bytes)
                .map(|value| VmValue::String(Rc::from(value.to_string())))
                .map_err(|error| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "secret_get: secret '{}' is not valid UTF-8: {error}",
                        secret_id
                    ))))
                })
        })
    });

    vm.register_async_builtin("event_log_emit", |args| async move {
        let topic_name = required_string_arg(&args, 0, "event_log_emit", "topic")?;
        let kind = required_string_arg(&args, 1, "event_log_emit", "kind")?;
        let payload = args
            .get(2)
            .map(vm_value_to_json)
            .unwrap_or(serde_json::Value::Null);
        let headers = optional_headers_arg(&args, 3, "event_log_emit")?;
        let ctx = active_harn_connector_ctx().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "event_log_emit: no active Harn connector context",
            )))
        })?;
        let topic = Topic::new(topic_name).map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "event_log_emit: invalid topic: {error}"
            ))))
        })?;
        let event_id = ctx
            .event_log
            .append(&topic, LogEvent::new(kind, payload).with_headers(headers))
            .await
            .map_err(|error| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "event_log_emit: {error}"
                ))))
            })?;
        Ok(VmValue::Int(event_id as i64))
    });

    vm.register_async_builtin("metrics_inc", |args| async move {
        let name = required_string_arg(&args, 0, "metrics_inc", "name")?;
        let amount = match args.get(1) {
            Some(VmValue::Int(value)) => *value,
            Some(VmValue::Float(value)) => *value as i64,
            Some(value) if !matches!(value, VmValue::Nil) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "metrics_inc: amount must be numeric, got {}",
                    value.type_name()
                )))));
            }
            _ => 1,
        };
        let ctx = active_harn_connector_ctx().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "metrics_inc: no active Harn connector context",
            )))
        })?;
        ctx.metrics
            .record_custom_counter(name.as_str(), amount.max(0) as u64);
        Ok(VmValue::Nil)
    });
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: {label} is required"
        )))));
    }
    Ok(value)
}

fn client_error_to_vm(error: ClientError) -> VmError {
    match error {
        ClientError::EgressBlocked(blocked) => blocked.to_vm_error(),
        other => VmError::Thrown(VmValue::String(Rc::from(other.to_string()))),
    }
}

fn optional_headers_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<BTreeMap<String, String>, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(dict)) => Ok(dict
            .iter()
            .map(|(key, value)| (key.clone(), value.display()))
            .collect()),
        Some(_other) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: headers must be a dict when provided"
        ))))),
    }
}

fn parse_secret_id(raw: &str) -> Option<SecretId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (base, version) = match trimmed.rsplit_once('@') {
        Some((base, raw_version)) => (base, SecretVersion::Exact(raw_version.parse::<u64>().ok()?)),
        None => (trimmed, SecretVersion::Latest),
    };
    let (namespace, name) = base.split_once('/')?;
    if namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(SecretId::new(namespace, name).with_version(version))
}
