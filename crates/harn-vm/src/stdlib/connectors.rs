use std::collections::BTreeMap;
use std::rc::Rc;

use jsonwebtoken::jwk::JwkSet;
use serde_json::Value as JsonValue;

use crate::bridge::json_result_to_vm_value;
use crate::connectors::{
    active_connector_client, harn_module::active_harn_connector_ctx, ClientError, JwtKeySource,
    JwtVerificationOptions,
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
                        "secret_get: secret '{secret_id}' is not valid UTF-8: {error}"
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

    vm.register_async_builtin("connector_shared_verify_jwt_inline", |args| async move {
        let token = required_string_arg(&args, 0, "connector_shared_verify_jwt_inline", "token")?;
        let jwks = required_json_arg(&args, 1, "connector_shared_verify_jwt_inline", "jwks")?;
        let options = optional_json_arg(&args, 2, "connector_shared_verify_jwt_inline")?;
        let jwks: JwkSet = serde_json::from_value(jwks).map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "connector_shared_verify_jwt_inline: invalid JWKS: {error}"
            ))))
        })?;
        let verify_options = jwt_verify_options(&options)?;
        let http = reqwest::Client::new();
        let result = crate::connectors::shared::verify_jwt_json(
            &http,
            &token,
            JwtKeySource::Inline(&jwks),
            &verify_options,
        )
        .await;
        let value = match result {
            Ok(claims) => serde_json::json!({
                "ok": true,
                "claims": claims,
                "error": null,
            }),
            Err(error) => serde_json::json!({
                "ok": false,
                "claims": null,
                "error": error.to_string(),
            }),
        };
        Ok(json_result_to_vm_value(&value))
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

fn required_json_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<JsonValue, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(_)) => Ok(vm_value_to_json(&args[index])),
        Some(value) if !matches!(value, VmValue::Nil) => {
            Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{builtin}: {label} must be a dict, got {}",
                value.type_name()
            )))))
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: {label} is required"
        ))))),
    }
}

fn optional_json_arg(args: &[VmValue], index: usize, builtin: &str) -> Result<JsonValue, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(_)) => Ok(vm_value_to_json(&args[index])),
        Some(VmValue::Nil) | None => Ok(JsonValue::Object(Default::default())),
        Some(value) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: options must be a dict, got {}",
            value.type_name()
        ))))),
    }
}

fn jwt_verify_options(options: &JsonValue) -> Result<JwtVerificationOptions, VmError> {
    let mut verify_options = JwtVerificationOptions::default();
    if let Some(issuer) = string_field(options, &["issuer", "iss"])? {
        verify_options = verify_options.with_issuer(issuer);
    }
    if let Some(audience) = string_field(options, &["audience", "aud"])? {
        verify_options = verify_options.with_audience(audience);
    }
    if let Some(algorithm) = string_field(options, &["algorithm", "alg"])? {
        verify_options = verify_options.with_algorithm(parse_jwt_algorithm(&algorithm)?);
    }
    let mut required = Vec::new();
    if options
        .get("require_exp")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        required.push("exp".to_string());
    }
    if verify_options.issuer.is_some() {
        required.push("iss".to_string());
    }
    if verify_options.audience.is_some() {
        required.push("aud".to_string());
    }
    if !required.is_empty() {
        verify_options = verify_options.require_spec_claims(required);
    }
    Ok(verify_options)
}

fn parse_jwt_algorithm(value: &str) -> Result<jsonwebtoken::Algorithm, VmError> {
    use jsonwebtoken::Algorithm;
    match value.trim() {
        "HS256" => Ok(Algorithm::HS256),
        "HS384" => Ok(Algorithm::HS384),
        "HS512" => Ok(Algorithm::HS512),
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        "ES256" => Ok(Algorithm::ES256),
        "ES384" => Ok(Algorithm::ES384),
        "PS256" => Ok(Algorithm::PS256),
        "PS384" => Ok(Algorithm::PS384),
        "PS512" => Ok(Algorithm::PS512),
        "EdDSA" => Ok(Algorithm::EdDSA),
        other => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "connector_shared_verify_jwt_inline: unsupported JWT algorithm `{other}`"
        ))))),
    }
}

fn string_field(options: &JsonValue, names: &[&str]) -> Result<Option<String>, VmError> {
    for name in names {
        match options.get(*name) {
            Some(JsonValue::String(value)) if !value.trim().is_empty() => {
                return Ok(Some(value.clone()));
            }
            Some(JsonValue::Null) | None => {}
            Some(value) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "connector_shared_verify_jwt_inline: option `{name}` must be a string, got {}",
                    json_type_name(value)
                )))));
            }
        }
    }
    Ok(None)
}

fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "nil",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "list",
        JsonValue::Object(_) => "dict",
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
