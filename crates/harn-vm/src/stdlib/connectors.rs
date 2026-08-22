use jsonwebtoken::jwk::JwkSet;
use serde_json::Value as JsonValue;

use crate::bridge::json_result_to_vm_value;
use crate::connectors::{
    active_connector_client, harn_module::active_harn_connector_ctx, ClientError, JwtKeySource,
    JwtVerificationOptions,
};
use crate::llm::vm_value_to_json;
use crate::stdlib::args::Args;
use crate::stdlib::macros::{harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_connector_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &CONNECTOR_CALL_IMPL_DEF,
    &METRICS_INC_IMPL_DEF,
    &CONNECTOR_SHARED_VERIFY_JWT_INLINE_IMPL_DEF,
];

#[harn_builtin(
    exposure = "harness.net.connector_call",
    effects = ["network.mutate@dynamic"],
    sig = "connector_call(provider: string, method: string, params?: dict) -> any",
    kind = "async",
    category = "connectors"
)]
async fn connector_call_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let call = Args::thrown("connector_call", &args);
    let provider = call.non_empty_string(0, "provider")?.to_string();
    let method = call.non_empty_string(1, "method")?.to_string();
    let params = optional_json_arg(&call, 2, "params")?;

    let client = active_connector_client(&provider).ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "connector_call: connector `{provider}` is not active"
        ))))
    })?;

    let result = client
        .call(&method, params)
        .await
        .map_err(client_error_to_vm)?;
    Ok(json_result_to_vm_value(&result))
}

#[harn_builtin(
    exposure = "harness.obs.metrics_inc",
    effects = ["observability.write@arg0"],
    sig_expr = BuiltinSignature::variadic("metrics_inc", &[Param::new("args", TY_ANY
)], TY_ANY),
    kind = "async",
    category = "connectors"
)]
async fn metrics_inc_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = Args::thrown("metrics_inc", &args)
        .non_empty_string(0, "name")?
        .to_string();
    let amount = match args.get(1) {
        Some(VmValue::Int(value)) => *value,
        Some(VmValue::Float(value)) => *value as i64,
        Some(value) if !matches!(value, VmValue::Nil) => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "metrics_inc: amount must be numeric, got {}",
                    value.type_name()
                ),
            ))));
        }
        _ => 1,
    };
    let ctx = active_harn_connector_ctx().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "metrics_inc: no active Harn connector context",
        )))
    })?;
    ctx.metrics
        .record_custom_counter(name.as_str(), amount.max(0) as u64);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "connector_shared_verify_jwt_inline(token: string, jwks: dict, options?: dict) -> dict",
    kind = "async",
    category = "connectors"
)]
async fn connector_shared_verify_jwt_inline_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let verify = Args::thrown("connector_shared_verify_jwt_inline", &args);
    let token = verify.non_empty_string(0, "token")?.to_string();
    let jwks = required_json_arg(&verify, 1, "jwks")?;
    let options = optional_json_arg(&verify, 2, "options")?;
    let jwks: JwkSet = serde_json::from_value(jwks).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "connector_shared_verify_jwt_inline: invalid JWKS: {error}"
        ))))
    })?;
    let verify_options = jwt_verify_options(&options)?;
    let http = crate::connectors::outbound_http_client("harn-connector-jwt-inline");
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
}

fn client_error_to_vm(error: ClientError) -> VmError {
    match error {
        ClientError::EgressBlocked(blocked) => blocked.to_vm_error(),
        other => VmError::Thrown(VmValue::String(arcstr::ArcStr::from(other.to_string()))),
    }
}

/// A required dict argument, as JSON.
///
/// The dict is type-checked through the shared contract first; only then is
/// the original value handed to the JSON converter.
fn required_json_arg(args: &Args<'_>, index: usize, label: &str) -> Result<JsonValue, VmError> {
    args.dict(index, label)?;
    Ok(vm_value_to_json(args.raw(index).expect("checked above")))
}

/// An optional dict argument, as JSON. Missing or `nil` yields `{}`.
fn optional_json_arg(args: &Args<'_>, index: usize, label: &str) -> Result<JsonValue, VmError> {
    match args.opt_dict(index, label)? {
        None => Ok(JsonValue::Object(Default::default())),
        Some(_) => Ok(vm_value_to_json(args.raw(index).expect("checked above"))),
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
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("connector_shared_verify_jwt_inline: unsupported JWT algorithm `{other}`"),
        )))),
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
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "connector_shared_verify_jwt_inline: option `{name}` must be a string, got {}",
                    json_type_name(value)
                ),
                ))));
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
