//! Webhook intake builtins: registering signed endpoints and feeding deliveries.
//!
//! Parses the intake configuration a script registers — signature header,
//! prefix, encoding, HMAC algorithm, shared secret, dedupe window — and the
//! delivery requests fed back in. Signature verification and dedupe live in
//! `crate::triggers::webhook_intake`; this module is the Harn-facing edge.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::stdlib::macros::harn_builtin;
use crate::triggers::{
    deregister_webhook_intake, feed_webhook_intake, recent_webhook_deliveries,
    register_webhook_intake, snapshot_webhook_intakes, HmacAlgorithm, SignatureEncoding,
    WebhookIntakeConfig, WebhookIntakeError, WebhookIntakeRequest,
};
use crate::value::{VmError, VmValue};

use super::args::{
    optional_bool, optional_string, require_dict_arg, required_string, required_string_arg,
    value_from_serde,
};

#[harn_builtin(
    sig = "webhook_intake_register(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn webhook_intake_register_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let config = require_dict_arg(&args, 0, "webhook_intake_register")?;
    let parsed = parse_webhook_intake_config(config)?;
    let snapshot = register_webhook_intake(parsed).map_err(webhook_intake_error)?;
    Ok(value_from_serde(&snapshot))
}

#[harn_builtin(
    sig = "webhook_intake_feed(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn webhook_intake_feed_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let intake_id = required_string_arg(&args, 0, "webhook_intake_feed", "intake_id")?;
    let request_dict = require_dict_arg(&args, 1, "webhook_intake_feed")?;
    let request = parse_webhook_intake_request(request_dict)?;
    let outcome = feed_webhook_intake(&intake_id, request)
        .await
        .map_err(webhook_intake_error)?;
    Ok(value_from_serde(&outcome))
}

#[harn_builtin(
    sig = "webhook_intake_deregister(...args: any) -> bool",
    kind = "async",
    category = "triggers"
)]
async fn webhook_intake_deregister_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let intake_id = required_string_arg(&args, 0, "webhook_intake_deregister", "intake_id")?;
    Ok(VmValue::Bool(deregister_webhook_intake(&intake_id)))
}

#[harn_builtin(
    sig = "webhook_intake_list(...args: any) -> list",
    category = "triggers"
)]
fn webhook_intake_list_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(
        snapshot_webhook_intakes()
            .into_iter()
            .map(|snapshot| value_from_serde(&snapshot))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "webhook_intake_recent(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn webhook_intake_recent_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let intake_id = required_string_arg(&args, 0, "webhook_intake_recent", "intake_id")?;
    let limit = match args.get(1) {
        Some(VmValue::Int(value)) if *value >= 0 => *value as usize,
        Some(VmValue::Nil) | None => 32,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_recent: limit must be a non-negative int, got {}",
                other.type_name()
            )))
        }
    };
    let entries = recent_webhook_deliveries(&intake_id, limit)
        .await
        .map_err(webhook_intake_error)?;
    Ok(VmValue::List(std::sync::Arc::new(
        entries
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    )))
}

fn webhook_intake_error(error: WebhookIntakeError) -> VmError {
    VmError::Runtime(format!("webhook_intake: {error}"))
}

fn parse_webhook_intake_config(
    config: &crate::value::DictMap,
) -> Result<WebhookIntakeConfig, VmError> {
    let signature_header = required_string(config, "signature_header", "webhook_intake_register")?;
    let delivery_id_header =
        required_string(config, "delivery_id_header", "webhook_intake_register")?;
    let topic = required_string(config, "topic", "webhook_intake_register")?;
    let allow_legacy_sha1 =
        optional_bool(config, "allow_legacy_sha1", "webhook_intake_register")?.unwrap_or(false);

    let algorithm = match optional_string(config, "algorithm").as_deref() {
        Some(raw) if HmacAlgorithm::is_legacy_sha1_alias(raw) && !allow_legacy_sha1 => {
            return Err(VmError::Runtime(
                "webhook_intake_register: algorithm `sha1` is legacy; set `allow_legacy_sha1: true` to verify HMAC-SHA1 signatures".to_string(),
            ));
        }
        Some(raw) => {
            HmacAlgorithm::parse_with_legacy_sha1(raw, allow_legacy_sha1).ok_or_else(|| {
                VmError::Runtime(format!(
                    "webhook_intake_register: unsupported algorithm `{raw}`, expected sha256"
                ))
            })?
        }
        None => HmacAlgorithm::default(),
    };
    let signature_encoding = match optional_string(config, "signature_encoding").as_deref() {
        Some(raw) => SignatureEncoding::parse(raw).ok_or_else(|| {
            VmError::Runtime(format!(
                "webhook_intake_register: unsupported signature_encoding `{raw}`, expected hex or base64"
            ))
        })?,
        None => SignatureEncoding::default(),
    };
    // `signature_prefix` defaults to "<algorithm>=" (matches GitHub, GitLab,
    // Bitbucket conventions). Pass an empty string explicitly to opt out.
    let signature_prefix = match config.get("signature_prefix") {
        Some(VmValue::String(text)) => Some(text.to_string()),
        Some(VmValue::Nil) | None => Some(format!("{}=", algorithm.as_str())),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_register: signature_prefix must be a string or nil, got {}",
                other.type_name()
            )))
        }
    }
    .filter(|prefix| !prefix.is_empty());

    let secret = parse_intake_secret(config)?;
    let dedupe_ttl_secs = match config.get("dedupe_ttl_seconds") {
        Some(VmValue::Int(value)) if *value > 0 => *value as u64,
        Some(VmValue::Nil) | None => crate::triggers::webhook_intake::DEFAULT_DEDUPE_TTL_SECS,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_register: dedupe_ttl_seconds must be a positive int, got {}",
                other.type_name()
            )))
        }
    };

    Ok(WebhookIntakeConfig {
        id: optional_string(config, "id"),
        path: optional_string(config, "path"),
        signature_header,
        signature_prefix,
        signature_encoding,
        algorithm,
        allow_legacy_sha1,
        secret,
        delivery_id_header,
        topic,
        dedupe_ttl: std::time::Duration::from_secs(dedupe_ttl_secs),
    })
}

fn parse_intake_secret(config: &crate::value::DictMap) -> Result<Vec<u8>, VmError> {
    match config.get("secret") {
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(VmValue::Bytes(bytes)) => Ok((**bytes).clone()),
        Some(other) => Err(VmError::Runtime(format!(
            "webhook_intake_register: `secret` must be a string or bytes, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(
            "webhook_intake_register: missing `secret` (string or bytes)".to_string(),
        )),
    }
}

fn parse_webhook_intake_request(
    request: &crate::value::DictMap,
) -> Result<WebhookIntakeRequest, VmError> {
    let headers = match request.get("headers") {
        Some(VmValue::Dict(dict)) => dict
            .iter()
            .map(|(key, value)| match value {
                VmValue::String(text) => Ok((key.to_string(), text.to_string())),
                VmValue::Int(value) => Ok((key.to_string(), value.to_string())),
                VmValue::Float(value) => Ok((key.to_string(), value.to_string())),
                VmValue::Bool(value) => Ok((key.to_string(), value.to_string())),
                other => Err(VmError::Runtime(format!(
                    "webhook_intake_feed: headers.{key} must be a string, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        Some(VmValue::Nil) | None => BTreeMap::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: headers must be a dict, got {}",
                other.type_name()
            )))
        }
    };
    let body = match request.get("body") {
        Some(VmValue::Bytes(bytes)) => (**bytes).clone(),
        Some(VmValue::String(text)) => text.as_bytes().to_vec(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: body must be string or bytes, got {}",
                other.type_name()
            )))
        }
    };
    let path = optional_string(request, "path");
    let received_at = match request.get("received_at") {
        Some(VmValue::String(text)) => Some(
            OffsetDateTime::parse(
                text.as_str(),
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|error| {
                VmError::Runtime(format!(
                    "webhook_intake_feed: received_at must be RFC3339, got `{text}`: {error}"
                ))
            })?,
        ),
        Some(VmValue::Nil) | None => None,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "webhook_intake_feed: received_at must be a string, got {}",
                other.type_name()
            )))
        }
    };
    Ok(WebhookIntakeRequest {
        headers,
        body,
        path,
        received_at,
    })
}
