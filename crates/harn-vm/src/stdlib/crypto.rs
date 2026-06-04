use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use url::Url;
use zeroize::Zeroizing;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::url_encoding::percent_encode_component;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn jwt_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("jwt_sign: {}", message.into()))
}

fn jwt_algorithm(name: &str) -> Result<Algorithm, VmError> {
    match name {
        "ES256" => Ok(Algorithm::ES256),
        "RS256" => Ok(Algorithm::RS256),
        other => Err(jwt_error(format!(
            "unsupported algorithm `{other}`; expected ES256 or RS256"
        ))),
    }
}

fn jwt_encoding_key(algorithm: Algorithm, pem: &str) -> Result<EncodingKey, VmError> {
    match algorithm {
        Algorithm::ES256 => EncodingKey::from_ec_pem(pem.as_bytes())
            .map_err(|error| jwt_error(format!("invalid ES256 PEM private key: {error}"))),
        Algorithm::RS256 => EncodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|error| jwt_error(format!("invalid RS256 PEM private key: {error}"))),
        _ => Err(jwt_error("unsupported algorithm")),
    }
}

fn jwt_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    if args.len() != 3 {
        return Err(jwt_error("requires 3 arguments: alg, claims, private_key"));
    }

    let alg = match &args[0] {
        VmValue::String(value) => value.as_ref(),
        other => {
            return Err(jwt_error(format!(
                "alg must be a string, got {}",
                other.type_name()
            )));
        }
    };
    let algorithm = jwt_algorithm(alg)?;

    if !matches!(args[1], VmValue::Dict(_)) {
        return Err(jwt_error(format!(
            "claims must be a dict, got {}",
            args[1].type_name()
        )));
    }
    let claims_json = super::json::vm_value_to_json(&args[1]);
    let claims: serde_json::Value = serde_json::from_str(&claims_json)
        .map_err(|error| jwt_error(format!("claims are not JSON-serializable: {error}")))?;

    let private_key = match &args[2] {
        VmValue::String(value) => Zeroizing::new(value.to_string()),
        other => {
            return Err(jwt_error(format!(
                "private_key must be a PEM string, got {}",
                other.type_name()
            )));
        }
    };
    let key = jwt_encoding_key(algorithm, private_key.as_ref())?;
    let token = jsonwebtoken::encode(&Header::new(algorithm), &claims, &key)
        .map_err(|error| jwt_error(format!("signing failed: {error}")))?;

    Ok(VmValue::String(std::sync::Arc::from(token)))
}

#[derive(Clone, Debug)]
struct SignedUrlOptions {
    signature_param: String,
    expires_param: String,
    kid_param: String,
    kid: Option<String>,
    skew_seconds: i64,
}

impl Default for SignedUrlOptions {
    fn default() -> Self {
        Self {
            signature_param: "sig".to_string(),
            expires_param: "exp".to_string(),
            kid_param: "kid".to_string(),
            kid: None,
            skew_seconds: 0,
        }
    }
}

#[derive(Debug)]
struct UrlParts {
    resource: String,
    output_prefix: String,
    query_pairs: Vec<(String, String)>,
}

fn signed_url_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("signed_url: {}", message.into()))
}

fn verify_signed_url_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("verify_signed_url: {}", message.into()))
}

fn string_arg<'a>(args: &'a [VmValue], index: usize, name: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.as_ref()),
        Some(other) => Err(signed_url_error(format!(
            "{name} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(signed_url_error(format!("{name} is required"))),
    }
}

fn int_arg(args: &[VmValue], index: usize, name: &str) -> Result<i64, VmError> {
    match args.get(index) {
        Some(VmValue::Int(value)) => Ok(*value),
        Some(other) => Err(signed_url_error(format!(
            "{name} must be an int, got {}",
            other.type_name()
        ))),
        None => Err(signed_url_error(format!("{name} is required"))),
    }
}

fn option_string(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match options.get(key) {
        Some(VmValue::String(value)) => Ok(Some(value.to_string())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: option `{key}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn option_int(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match options.get(key) {
        Some(VmValue::Int(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: option `{key}` must be an int, got {}",
            other.type_name()
        ))),
    }
}

fn signed_url_options(value: Option<&VmValue>, builtin: &str) -> Result<SignedUrlOptions, VmError> {
    let mut options = SignedUrlOptions::default();
    let Some(value) = value else {
        return Ok(options);
    };
    let dict = value.as_dict().ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: options must be a dict, got {}",
            value.type_name()
        ))
    })?;
    if let Some(signature_param) = option_string(dict, "signature_param", builtin)? {
        options.signature_param = signature_param;
    }
    if let Some(expires_param) = option_string(dict, "expires_param", builtin)? {
        options.expires_param = expires_param;
    }
    if let Some(kid_param) = option_string(dict, "kid_param", builtin)? {
        options.kid_param = kid_param;
    }
    options.kid = option_string(dict, "kid", builtin)?;
    options.skew_seconds = option_int(dict, "skew_seconds", builtin)?.unwrap_or(0);
    validate_param_name(&options.signature_param, builtin, "signature_param")?;
    validate_param_name(&options.expires_param, builtin, "expires_param")?;
    validate_param_name(&options.kid_param, builtin, "kid_param")?;
    if options.signature_param == options.expires_param
        || options.signature_param == options.kid_param
        || options.expires_param == options.kid_param
    {
        return Err(VmError::Runtime(format!(
            "{builtin}: signature_param, expires_param, and kid_param must be distinct"
        )));
    }
    if options.skew_seconds < 0 {
        return Err(VmError::Runtime(format!(
            "{builtin}: skew_seconds must be greater than or equal to 0"
        )));
    }
    Ok(options)
}

fn validate_param_name(param: &str, builtin: &str, option_name: &str) -> Result<(), VmError> {
    if param.is_empty() {
        return Err(VmError::Runtime(format!(
            "{builtin}: option `{option_name}` cannot be empty"
        )));
    }
    Ok(())
}

fn parse_url_parts(raw: &str, builtin: &str) -> Result<UrlParts, VmError> {
    if let Ok(mut parsed) = Url::parse(raw) {
        if parsed.cannot_be_a_base() || parsed.host_str().is_none() {
            return Err(VmError::Runtime(format!(
                "{builtin}: expected an absolute URL with a host or an absolute path starting with `/`"
            )));
        }
        parsed.set_fragment(None);
        let query_pairs = parsed.query().map(parse_query_pairs).unwrap_or_default();
        parsed.set_query(None);
        let origin = parsed.origin().ascii_serialization();
        let path = parsed.path();
        let resource = format!("{origin}{path}");
        return Ok(UrlParts {
            resource,
            output_prefix: parsed.as_str().to_string(),
            query_pairs,
        });
    }

    let without_fragment = raw.split_once('#').map(|(head, _)| head).unwrap_or(raw);
    let (path, query) = without_fragment
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((without_fragment, None));
    let path = if path.is_empty() { "/" } else { path };
    if !path.starts_with('/') {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected an absolute URL or an absolute path starting with `/`"
        )));
    }
    let path = percent_encode_path(path);
    Ok(UrlParts {
        resource: path.clone(),
        output_prefix: path,
        query_pairs: query.map(parse_query_pairs).unwrap_or_default(),
    })
}

fn parse_query_pairs(raw: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn percent_encode_path(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && (bytes[i + 1] as char).is_ascii_hexdigit()
                && (bytes[i + 2] as char).is_ascii_hexdigit() =>
            {
                out.push('%');
                out.push((bytes[i + 1] as char).to_ascii_uppercase());
                out.push((bytes[i + 2] as char).to_ascii_uppercase());
                i += 3;
            }
            _ => {
                out.push_str(&format!("%{byte:02X}"));
                i += 1;
            }
        }
    }
    out
}

fn canonical_query(pairs: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| {
            (
                percent_encode_component(key),
                percent_encode_component(value),
            )
        })
        .collect();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn signed_url_payload(resource: &str, query: &str) -> String {
    format!("harn-signed-url-v1\n{resource}\n{query}")
}

fn hmac_sha256_base64url(secret: &str, message: &str) -> String {
    use base64::Engine;
    let mac = crate::connectors::hmac::hmac_sha256(secret.as_bytes(), message.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac)
}

fn append_query(prefix: &str, query: &str) -> String {
    if query.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}?{query}")
    }
}

fn claims_arg<'a>(
    args: &'a [VmValue],
    options: &SignedUrlOptions,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    let Some(value) = args.get(1) else {
        return Err(signed_url_error("claims is required"));
    };
    let dict = value.as_dict().ok_or_else(|| {
        signed_url_error(format!("claims must be a dict, got {}", value.type_name()))
    })?;
    for reserved in [
        &options.signature_param,
        &options.expires_param,
        &options.kid_param,
    ] {
        if dict.contains_key(reserved) {
            return Err(signed_url_error(format!(
                "claims cannot contain reserved parameter `{reserved}`"
            )));
        }
    }
    Ok(dict)
}

fn signed_url_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    if args.len() < 4 || args.len() > 5 {
        return Err(signed_url_error(
            "requires 4 or 5 arguments: base, claims, secret, expires_at, options?",
        ));
    }
    let options = signed_url_options(args.get(4), "signed_url")?;
    let base = string_arg(args, 0, "base")?;
    let claims = claims_arg(args, &options)?;
    let secret = string_arg(args, 2, "secret")?;
    let expires_at = int_arg(args, 3, "expires_at")?;
    let mut parts = parse_url_parts(base, "signed_url")?;

    parts.query_pairs.retain(|(key, _)| {
        key != &options.signature_param
            && key != &options.expires_param
            && key != &options.kid_param
    });
    for (key, value) in claims.iter() {
        parts.query_pairs.push((key.clone(), value.display()));
    }
    parts
        .query_pairs
        .push((options.expires_param.clone(), expires_at.to_string()));
    if let Some(kid) = &options.kid {
        parts
            .query_pairs
            .push((options.kid_param.clone(), kid.clone()));
    }

    let query_without_signature = canonical_query(&parts.query_pairs);
    let payload = signed_url_payload(&parts.resource, &query_without_signature);
    let signature = hmac_sha256_base64url(secret, &payload);
    let mut signed_pairs = parts.query_pairs;
    signed_pairs.push((options.signature_param, signature));
    let signed_query = canonical_query(&signed_pairs);
    Ok(VmValue::String(std::sync::Arc::from(append_query(
        &parts.output_prefix,
        &signed_query,
    ))))
}

fn verification_result(
    valid: bool,
    reason: &str,
    signature_valid: bool,
    expired: bool,
    expires_at: Option<i64>,
    kid: Option<String>,
    claims: BTreeMap<String, VmValue>,
) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("valid".to_string(), VmValue::Bool(valid));
    result.insert(
        "reason".to_string(),
        VmValue::String(std::sync::Arc::from(reason)),
    );
    result.insert(
        "signature_valid".to_string(),
        VmValue::Bool(signature_valid),
    );
    result.insert("expired".to_string(), VmValue::Bool(expired));
    result.insert(
        "expires_at".to_string(),
        expires_at.map(VmValue::Int).unwrap_or(VmValue::Nil),
    );
    result.insert(
        "kid".to_string(),
        kid.map(|kid| VmValue::String(std::sync::Arc::from(kid)))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        "claims".to_string(),
        VmValue::Dict(std::sync::Arc::new(claims)),
    );
    VmValue::Dict(std::sync::Arc::new(result))
}

fn choose_secret(secret_or_keys: &VmValue, kid: Option<&str>) -> Result<Option<String>, VmError> {
    match secret_or_keys {
        VmValue::String(secret) => Ok(Some(secret.to_string())),
        VmValue::Dict(keys) => {
            let Some(kid) = kid else {
                return Ok(None);
            };
            Ok(keys.get(kid).map(|secret| secret.display()))
        }
        other => Err(verify_signed_url_error(format!(
            "secret_or_keys must be a string or dict, got {}",
            other.type_name()
        ))),
    }
}

fn verify_signed_url_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(verify_signed_url_error(
            "requires 3 or 4 arguments: url, secret_or_keys, now, options?",
        ));
    }
    let options = signed_url_options(args.get(3), "verify_signed_url")?;
    let raw_url = match args.first() {
        Some(VmValue::String(value)) => value.as_ref(),
        Some(other) => {
            return Err(verify_signed_url_error(format!(
                "url must be a string, got {}",
                other.type_name()
            )));
        }
        None => return Err(verify_signed_url_error("url is required")),
    };
    let now = match args.get(2) {
        Some(VmValue::Int(value)) => *value,
        Some(other) => {
            return Err(verify_signed_url_error(format!(
                "now must be an int, got {}",
                other.type_name()
            )));
        }
        None => return Err(verify_signed_url_error("now is required")),
    };
    let mut parts = parse_url_parts(raw_url, "verify_signed_url")?;

    let signatures: Vec<String> = parts
        .query_pairs
        .iter()
        .filter(|(key, _)| key == &options.signature_param)
        .map(|(_, value)| value.clone())
        .collect();
    if signatures.len() != 1 {
        return Ok(verification_result(
            false,
            "missing_signature",
            false,
            false,
            None,
            None,
            BTreeMap::new(),
        ));
    }
    let signature = signatures[0].clone();

    let expires_values: Vec<String> = parts
        .query_pairs
        .iter()
        .filter(|(key, _)| key == &options.expires_param)
        .map(|(_, value)| value.clone())
        .collect();
    if expires_values.len() != 1 {
        return Ok(verification_result(
            false,
            "missing_expiry",
            false,
            false,
            None,
            None,
            BTreeMap::new(),
        ));
    }
    let expires_at = match expires_values[0].parse::<i64>() {
        Ok(value) => value,
        Err(_) => {
            return Ok(verification_result(
                false,
                "invalid_expiry",
                false,
                false,
                None,
                None,
                BTreeMap::new(),
            ));
        }
    };

    let kid_values: Vec<String> = parts
        .query_pairs
        .iter()
        .filter(|(key, _)| key == &options.kid_param)
        .map(|(_, value)| value.clone())
        .collect();
    if kid_values.len() > 1 {
        return Ok(verification_result(
            false,
            "duplicate_kid",
            false,
            false,
            Some(expires_at),
            None,
            BTreeMap::new(),
        ));
    }
    let kid = kid_values.first().cloned();

    let mut claims = BTreeMap::new();
    for (key, value) in &parts.query_pairs {
        if key != &options.signature_param
            && key != &options.expires_param
            && key != &options.kid_param
        {
            claims.insert(
                key.clone(),
                VmValue::String(std::sync::Arc::from(value.as_str())),
            );
        }
    }

    let Some(secret_or_keys) = args.get(1) else {
        return Err(verify_signed_url_error("secret_or_keys is required"));
    };
    let Some(secret) = choose_secret(secret_or_keys, kid.as_deref())? else {
        return Ok(verification_result(
            false,
            "unknown_key",
            false,
            false,
            Some(expires_at),
            kid,
            claims,
        ));
    };

    parts
        .query_pairs
        .retain(|(key, _)| key != &options.signature_param);
    let query_without_signature = canonical_query(&parts.query_pairs);
    let payload = signed_url_payload(&parts.resource, &query_without_signature);
    let expected = hmac_sha256_base64url(&secret, &payload);
    let signature_valid =
        crate::connectors::hmac::secure_eq(signature.as_bytes(), expected.as_bytes());
    let expired = now > expires_at.saturating_add(options.skew_seconds);
    let valid = signature_valid && !expired;
    let reason = if !signature_valid {
        "bad_signature"
    } else if expired {
        "expired"
    } else {
        "ok"
    };
    Ok(verification_result(
        valid,
        reason,
        signature_valid,
        expired,
        Some(expires_at),
        kid,
        claims,
    ))
}

fn aws_sigv4_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("aws_sigv4_headers: {}", message.into()))
}

fn required_spec_string(spec: &BTreeMap<String, VmValue>, key: &str) -> Result<String, VmError> {
    match spec.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        Some(value) if !matches!(value, VmValue::Nil) => Err(aws_sigv4_error(format!(
            "{key} must be a non-empty string, got {}",
            value.type_name()
        ))),
        _ => Err(aws_sigv4_error(format!("{key} is required"))),
    }
}

fn optional_spec_string(
    spec: &BTreeMap<String, VmValue>,
    key: &str,
) -> Result<Option<String>, VmError> {
    match spec.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.to_string())),
        Some(VmValue::String(_)) | Some(VmValue::Nil) | None => Ok(None),
        Some(value) => Err(aws_sigv4_error(format!(
            "{key} must be a string when provided, got {}",
            value.type_name()
        ))),
    }
}

fn aws_sigv4_headers_arg(
    spec: &BTreeMap<String, VmValue>,
) -> Result<BTreeMap<String, String>, VmError> {
    match spec.get("headers") {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(headers)) => Ok(headers
            .iter()
            .map(|(key, value)| (key.clone(), value.display()))
            .collect()),
        Some(value) => Err(aws_sigv4_error(format!(
            "headers must be a dict when provided, got {}",
            value.type_name()
        ))),
    }
}

fn aws_sigv4_body_bytes(spec: &BTreeMap<String, VmValue>) -> Vec<u8> {
    match spec.get("body") {
        Some(VmValue::Bytes(bytes)) => bytes.as_ref().clone(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(value) => value.display().into_bytes(),
    }
}

fn aws_sigv4_timestamp(value: Option<&VmValue>) -> Result<DateTime<Utc>, VmError> {
    let value = value.ok_or_else(|| aws_sigv4_error("timestamp is required"))?;
    match value {
        VmValue::String(text) => aws_sigv4_timestamp_string(text),
        VmValue::Int(seconds) => Utc
            .timestamp_opt(*seconds, 0)
            .single()
            .ok_or_else(|| aws_sigv4_error("timestamp is out of range")),
        VmValue::Float(seconds) => aws_sigv4_timestamp_float(*seconds),
        VmValue::Dict(dict) => aws_sigv4_timestamp(
            dict.get("timestamp").or_else(|| dict.get("iso8601")),
        ),
        other => Err(aws_sigv4_error(format!(
            "timestamp must be an RFC 3339 string, SigV4 basic timestamp, Unix timestamp, or date dict, got {}",
            other.type_name()
        ))),
    }
}

fn aws_sigv4_timestamp_string(text: &str) -> Result<DateTime<Utc>, VmError> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Ok(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%SZ")
        .map(|dt| Utc.from_utc_datetime(&dt))
        .map_err(|_| aws_sigv4_error("timestamp must be RFC 3339 or YYYYMMDDTHHMMSSZ"))
}

fn aws_sigv4_timestamp_float(seconds: f64) -> Result<DateTime<Utc>, VmError> {
    if !seconds.is_finite() {
        return Err(aws_sigv4_error("timestamp must be finite"));
    }
    let mut secs = seconds.trunc() as i64;
    let mut nanos = ((seconds - secs as f64) * 1_000_000_000.0).round() as i64;
    if nanos < 0 {
        secs -= 1;
        nanos += 1_000_000_000;
    } else if nanos >= 1_000_000_000 {
        secs += 1;
        nanos -= 1_000_000_000;
    }
    Utc.timestamp_opt(secs, nanos as u32)
        .single()
        .ok_or_else(|| aws_sigv4_error("timestamp is out of range"))
}

fn aws_sigv4_headers_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    if args.len() != 1 {
        return Err(aws_sigv4_error("requires one spec dict argument"));
    }
    let spec = args[0].as_dict().ok_or_else(|| {
        aws_sigv4_error(format!("spec must be a dict, got {}", args[0].type_name()))
    })?;
    let credentials = crate::aws_sigv4::AwsSigV4Credentials {
        access_key_id: required_spec_string(spec, "access_key_id")?,
        secret_access_key: required_spec_string(spec, "secret_access_key")?,
        session_token: optional_spec_string(spec, "session_token")?,
    };
    let method = required_spec_string(spec, "method")?;
    let url = required_spec_string(spec, "url")?;
    let service = required_spec_string(spec, "service")?;
    let region = required_spec_string(spec, "region")?;
    let headers = aws_sigv4_headers_arg(spec)?;
    let body = aws_sigv4_body_bytes(spec);
    let timestamp = aws_sigv4_timestamp(spec.get("timestamp"))?;

    let signed = crate::aws_sigv4::sign(crate::aws_sigv4::AwsSigV4Input {
        credentials: &credentials,
        method: &method,
        url: &url,
        service: &service,
        region: &region,
        headers: &headers,
        body: &body,
        timestamp,
    })
    .map_err(aws_sigv4_error)?;

    Ok(crate::schema::json_to_vm_value(&serde_json::json!({
        "headers": signed.headers,
        "authorization": signed.authorization,
        "amz_date": signed.amz_date,
        "content_sha256": signed.content_sha256,
        "signed_headers": signed.signed_headers,
        "credential_scope": signed.credential_scope,
        "canonical_request": signed.canonical_request,
        "string_to_sign": signed.string_to_sign,
    })))
}

fn display_arg(args: &[VmValue]) -> String {
    args.first().map(|a| a.display()).unwrap_or_default()
}

fn bytes_or_string_input(arg: Option<&VmValue>) -> Result<Vec<u8>, VmError> {
    match arg {
        Some(VmValue::Bytes(b)) => Ok(b.to_vec()),
        Some(VmValue::String(s)) => Ok(s.as_bytes().to_vec()),
        Some(other) => Ok(other.display().into_bytes()),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn register_crypto_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "base64_encode(input: string | bytes) -> string",
    category = "crypto"
)]
fn base64_encode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = bytes_or_string_input(args.first())?;
    use base64::Engine;
    Ok(VmValue::String(std::sync::Arc::from(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    )))
}

#[harn_builtin(sig = "base64_decode(text: string?) -> string", category = "crypto")]
fn base64_decode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    use base64::Engine;
    match base64::engine::general_purpose::STANDARD.decode(val.as_bytes()) {
        Ok(bytes) => Ok(VmValue::String(std::sync::Arc::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(e) => Err(VmError::Runtime(format!("base64 decode error: {e}"))),
    }
}

#[harn_builtin(
    sig = "base64url_encode(input: string | bytes) -> string",
    category = "crypto"
)]
fn base64url_encode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = bytes_or_string_input(args.first())?;
    use base64::Engine;
    Ok(VmValue::String(std::sync::Arc::from(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
    )))
}

#[harn_builtin(sig = "base64url_decode(text: string?) -> string", category = "crypto")]
fn base64url_decode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    use base64::Engine;
    match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(val.as_bytes()) {
        Ok(bytes) => Ok(VmValue::String(std::sync::Arc::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(e) => Err(VmError::Runtime(format!("base64url decode error: {e}"))),
    }
}

// bytes_to_base64url(bytes) -> base64url-no-pad string. Accepts bytes
// or a string (treated as UTF-8 bytes). Use for PKCE S256 challenges,
// JWT segments, and any other URL-safe encoding of raw bytes.
#[harn_builtin(
    sig = "bytes_to_base64url(input: string | bytes) -> string",
    category = "crypto"
)]
fn bytes_to_base64url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
    )))
}

// sha256_base64url(input) -> base64url-no-pad encoding of SHA-256.
// Convenience for RFC 7636 PKCE S256 challenges so callers do not
// round-trip through hex.
#[harn_builtin(
    sig = "sha256_base64url(input: string | bytes) -> string",
    category = "crypto"
)]
fn sha256_base64url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(VmValue::String(std::sync::Arc::from(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest),
    )))
}

// crypto_random_bytes(n) -> VmValue::Bytes of length n drawn from a
// CSPRNG. Used by OAuth.client to build PKCE verifiers and state
// tokens with strong entropy. n must be a positive integer.
#[harn_builtin(sig = "crypto_random_bytes(n: int) -> bytes", category = "crypto")]
fn crypto_random_bytes_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::Rng as _;
    let n = match args.first() {
        Some(VmValue::Int(value)) if *value > 0 => *value as usize,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "crypto_random_bytes: n must be a positive integer, got {}",
                other.type_name()
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "crypto_random_bytes: n is required".to_string(),
            ));
        }
    };
    if n > 1024 {
        return Err(VmError::Runtime(format!(
            "crypto_random_bytes: n must be <= 1024, got {n}"
        )));
    }
    let mut out = vec![0u8; n];
    rand::rng().fill_bytes(&mut out);
    Ok(VmValue::Bytes(std::sync::Arc::new(out)))
}

#[harn_builtin(
    sig = "base32_encode(input: string | bytes) -> string",
    category = "crypto"
)]
fn base32_encode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(
        data_encoding::BASE32.encode(&bytes),
    )))
}

#[harn_builtin(sig = "base32_decode(text: string?) -> string", category = "crypto")]
fn base32_decode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    match data_encoding::BASE32.decode(val.as_bytes()) {
        Ok(bytes) => Ok(VmValue::String(std::sync::Arc::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(e) => Err(VmError::Runtime(format!("base32 decode error: {e}"))),
    }
}

#[harn_builtin(
    sig = "hex_encode(input: string | bytes) -> string",
    category = "crypto"
)]
fn hex_encode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(bytes))))
}

#[harn_builtin(sig = "hex_decode(text: string?) -> string", category = "crypto")]
fn hex_decode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    match hex::decode(val.as_bytes()) {
        Ok(bytes) => Ok(VmValue::String(std::sync::Arc::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(e) => Err(VmError::Runtime(format!("hex decode error: {e}"))),
    }
}

// Stable FNV-1a over the canonical display form so logically-equal values
// hash identically. For bucketing/indexing only — use sha256 for integrity.
#[harn_builtin(sig = "hash_value(value: any) -> int", category = "crypto")]
fn hash_value_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    let key = crate::value::value_structural_hash_key(val);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(VmValue::Int(hash as i64))
}

#[harn_builtin(sig = "sha256(input: string | bytes) -> string", category = "crypto")]
fn sha256_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sha2::Sha256::digest(&bytes),
    ))))
}

#[harn_builtin(sig = "sha224(input: string | bytes) -> string", category = "crypto")]
fn sha224_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sha2::Sha224::digest(&bytes),
    ))))
}

#[harn_builtin(sig = "sha384(input: string | bytes) -> string", category = "crypto")]
fn sha384_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sha2::Sha384::digest(&bytes),
    ))))
}

#[harn_builtin(sig = "sha512(input: string | bytes) -> string", category = "crypto")]
fn sha512_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sha2::Sha512::digest(&bytes),
    ))))
}

#[harn_builtin(
    sig = "sha512_256(input: string | bytes) -> string",
    category = "crypto"
)]
fn sha512_256_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha2::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sha2::Sha512_256::digest(&bytes),
    ))))
}

#[harn_builtin(sig = "md5(input: string | bytes) -> string", category = "crypto")]
fn md5_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use md5::Digest;
    let bytes = bytes_or_string_input(args.first())?;
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        md5::Md5::digest(&bytes),
    ))))
}

// Top-level alias for `harness.crypto.sha256(...)`. It accepts `Bytes`
// values directly, so content-addressing scripts can hash binary payloads
// without first stringifying them.
#[harn_builtin(
    sig = "sha256_hex(input: string | bytes) -> string",
    category = "crypto"
)]
fn sha256_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(crate::harness_crypto::sha256_hex_value(args))
}

// HMAC-SHA256 over (key, message). Both inputs are taken as their byte
// sequences (string `display()` of nil/numbers stringifies first). The
// returned hex string is what most webhook providers send in their
// signature header (e.g. GitHub's `x-hub-signature-256: sha256=<hex>`).
#[harn_builtin(
    sig = "hmac_sha256(key: string, message: string) -> string",
    category = "crypto"
)]
fn hmac_sha256_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    let msg = args.get(1).map(|a| a.display()).unwrap_or_default();
    let mac = crate::connectors::hmac::hmac_sha256(key.as_bytes(), msg.as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    Ok(VmValue::String(std::sync::Arc::from(hex)))
}

// HMAC-SHA256 returning standard base64 (used by Slack-style signatures).
#[harn_builtin(
    sig = "hmac_sha256_base64(key: string, message: string) -> string",
    category = "crypto"
)]
fn hmac_sha256_base64_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    let msg = args.get(1).map(|a| a.display()).unwrap_or_default();
    let mac = crate::connectors::hmac::hmac_sha256(key.as_bytes(), msg.as_bytes());
    Ok(VmValue::String(std::sync::Arc::from(
        base64::engine::general_purpose::STANDARD.encode(&mac),
    )))
}

// HMAC-SHA1 (hex). Provided for legacy webhook signatures (e.g. older
// Bitbucket / GitHub `x-hub-signature: sha1=<hex>`); SHA-256 should be
// preferred for any new integration.
#[harn_builtin(
    sig = "hmac_sha1(key: string, message: string) -> string",
    category = "crypto"
)]
fn hmac_sha1_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    let msg = args.get(1).map(|a| a.display()).unwrap_or_default();
    let mac = crate::connectors::hmac::hmac_sha1(key.as_bytes(), msg.as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    Ok(VmValue::String(std::sync::Arc::from(hex)))
}

#[harn_builtin(
    sig = "jwt_sign(algorithm: string, claims: dict, private_key: string) -> string",
    category = "crypto"
)]
fn jwt_sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    jwt_sign_builtin(args)
}

#[harn_builtin(
    sig = "signed_url(base: string?, claims: dict, secret: string?, expires_at: number, options?: dict) -> string",
    category = "crypto"
)]
fn signed_url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    signed_url_builtin(args)
}

#[harn_builtin(
    sig = "verify_signed_url(url: string?, secret_or_keys: string | dict, now: number, options?: dict) -> dict",
    category = "crypto"
)]
fn verify_signed_url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    verify_signed_url_builtin(args)
}

#[harn_builtin(sig = "aws_sigv4_headers(spec: dict) -> dict", category = "crypto")]
fn aws_sigv4_headers_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    aws_sigv4_headers_builtin(args)
}

// Constant-time string equality. The variable-time `==` operator can leak
// the position of the first differing byte through timing, which lets an
// attacker recover an HMAC signature byte-by-byte. Always use this for
// signature comparison.
#[harn_builtin(
    sig = "constant_time_eq(a: string, b: string) -> bool",
    category = "crypto"
)]
fn constant_time_eq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = args.first().map(|a| a.display()).unwrap_or_default();
    let b = args.get(1).map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::Bool(crate::connectors::hmac::secure_eq(
        a.as_bytes(),
        b.as_bytes(),
    )))
}

#[harn_builtin(sig = "url_encode(input: string) -> string", category = "crypto")]
fn url_encode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    let encoded: String = val
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    Ok(VmValue::String(std::sync::Arc::from(encoded)))
}

#[harn_builtin(sig = "url_decode(text: string?) -> string", category = "crypto")]
fn url_decode_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = display_arg(args);
    let mut result = Vec::new();
    let bytes = val.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&val[i + 1..i + 3], 16) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            result.push(b' ');
        } else {
            result.push(bytes[i]);
        }
        i += 1;
    }
    Ok(VmValue::String(std::sync::Arc::from(
        String::from_utf8_lossy(&result).into_owned(),
    )))
}

// --- modern hashing -------------------------------------------------

#[harn_builtin(sig = "sha3_256(input: string | bytes) -> string", category = "crypto")]
fn sha3_256_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha3::{Digest, Sha3_256};
    let input = bytes_or_string_input(args.first())?;
    let digest = Sha3_256::digest(&input);
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(digest))))
}

#[harn_builtin(sig = "sha3_512(input: string | bytes) -> string", category = "crypto")]
fn sha3_512_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use sha3::{Digest, Sha3_512};
    let input = bytes_or_string_input(args.first())?;
    let digest = Sha3_512::digest(&input);
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(digest))))
}

#[harn_builtin(sig = "blake3(input: string | bytes) -> string", category = "crypto")]
fn blake3_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = bytes_or_string_input(args.first())?;
    let digest = blake3::hash(&input);
    Ok(VmValue::String(std::sync::Arc::from(
        digest.to_hex().to_string(),
    )))
}

// --- ed25519 keypair / sign / verify --------------------------------

#[harn_builtin(sig = "ed25519_keypair() -> dict", category = "crypto")]
fn ed25519_keypair_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use ed25519_dalek::{SigningKey, VerifyingKey};
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    let signing = SigningKey::from_bytes(&bytes);
    let verifying: VerifyingKey = signing.verifying_key();
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(
        "private".to_string(),
        VmValue::String(std::sync::Arc::from(hex::encode(signing.to_bytes()))),
    );
    dict.insert(
        "public".to_string(),
        VmValue::String(std::sync::Arc::from(hex::encode(verifying.to_bytes()))),
    );
    Ok(VmValue::Dict(std::sync::Arc::new(dict)))
}

#[harn_builtin(
    sig = "ed25519_sign(private_hex: string, message: string | bytes) -> string",
    category = "crypto"
)]
fn ed25519_sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use ed25519_dalek::{Signer, SigningKey};
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "ed25519_sign: expected (private_hex, message)",
        ))));
    }
    let priv_hex = args[0].display();
    let msg = bytes_or_string_input(Some(&args[1]))?;
    let priv_bytes = hex::decode(&priv_hex).map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "ed25519_sign: invalid hex private key: {e}"
        ))))
    })?;
    let priv_arr: [u8; 32] = priv_bytes.as_slice().try_into().map_err(|_| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "ed25519_sign: private key must be 32 bytes",
        )))
    })?;
    let signing = SigningKey::from_bytes(&priv_arr);
    let sig = signing.sign(&msg);
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        sig.to_bytes(),
    ))))
}

#[harn_builtin(
    sig = "ed25519_verify(public_hex: string, message: string | bytes, signature_hex: string) -> bool",
    category = "crypto"
)]
fn ed25519_verify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if args.len() < 3 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "ed25519_verify: expected (public_hex, message, signature_hex)",
        ))));
    }
    let pub_hex = args[0].display();
    let msg = bytes_or_string_input(Some(&args[1]))?;
    let sig_hex = args[2].display();

    let pub_bytes = match hex::decode(&pub_hex) {
        Ok(b) => b,
        Err(_) => return Ok(VmValue::Bool(false)),
    };
    let sig_bytes = match hex::decode(&sig_hex) {
        Ok(b) => b,
        Err(_) => return Ok(VmValue::Bool(false)),
    };
    let pub_arr: [u8; 32] = match pub_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return Ok(VmValue::Bool(false)),
    };
    let sig_arr: [u8; 64] = match sig_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return Ok(VmValue::Bool(false)),
    };
    let verifying = match VerifyingKey::from_bytes(&pub_arr) {
        Ok(v) => v,
        Err(_) => return Ok(VmValue::Bool(false)),
    };
    let signature = Signature::from_bytes(&sig_arr);
    Ok(VmValue::Bool(verifying.verify(&msg, &signature).is_ok()))
}

// --- x25519 keypair / agree -----------------------------------------

#[harn_builtin(sig = "x25519_keypair() -> dict", category = "crypto")]
fn x25519_keypair_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::RngExt;
    use x25519_dalek::{PublicKey, StaticSecret};
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    let secret = StaticSecret::from(bytes);
    let public = PublicKey::from(&secret);
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(
        "private".to_string(),
        VmValue::String(std::sync::Arc::from(hex::encode(secret.to_bytes()))),
    );
    dict.insert(
        "public".to_string(),
        VmValue::String(std::sync::Arc::from(hex::encode(public.to_bytes()))),
    );
    Ok(VmValue::Dict(std::sync::Arc::new(dict)))
}

#[harn_builtin(
    sig = "x25519_agree(private_hex: string, peer_public_hex: string) -> string",
    category = "crypto"
)]
fn x25519_agree_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use x25519_dalek::{PublicKey, StaticSecret};
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "x25519_agree: expected (private_hex, peer_public_hex)",
        ))));
    }
    let priv_hex = args[0].display();
    let pub_hex = args[1].display();
    let priv_bytes = hex::decode(&priv_hex).map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "x25519_agree: invalid private hex: {e}"
        ))))
    })?;
    let pub_bytes = hex::decode(&pub_hex).map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "x25519_agree: invalid public hex: {e}"
        ))))
    })?;
    let priv_arr: [u8; 32] = priv_bytes.as_slice().try_into().map_err(|_| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "x25519_agree: private must be 32 bytes",
        )))
    })?;
    let pub_arr: [u8; 32] = pub_bytes.as_slice().try_into().map_err(|_| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "x25519_agree: public must be 32 bytes",
        )))
    })?;
    let secret = StaticSecret::from(priv_arr);
    let peer = PublicKey::from(pub_arr);
    let shared = secret.diffie_hellman(&peer);
    Ok(VmValue::String(std::sync::Arc::from(hex::encode(
        shared.as_bytes(),
    ))))
}

// --- jwt_verify (HS256 / RS256 / ES256) -----------------------------

#[harn_builtin(
    sig = "jwt_verify(algorithm: string, token: string, key: string) -> dict",
    category = "crypto"
)]
fn jwt_verify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use jsonwebtoken::{decode, DecodingKey, Validation};
    if args.len() < 3 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "jwt_verify: expected (alg, token, key)",
        ))));
    }
    let alg = args[0].display();
    let token = args[1].display();
    let key_str = args[2].display();
    let algorithm = match alg.as_str() {
        "HS256" => Algorithm::HS256,
        "ES256" => Algorithm::ES256,
        "RS256" => Algorithm::RS256,
        other => {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                format!("jwt_verify: unsupported algorithm '{other}'"),
            ))));
        }
    };
    let decoding_key = match algorithm {
        Algorithm::HS256 => DecodingKey::from_secret(key_str.as_bytes()),
        Algorithm::ES256 => DecodingKey::from_ec_pem(key_str.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "jwt_verify: invalid ES256 public key: {e}"
            ))))
        })?,
        Algorithm::RS256 => DecodingKey::from_rsa_pem(key_str.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "jwt_verify: invalid RS256 public key: {e}"
            ))))
        })?,
        _ => unreachable!(),
    };
    let mut validation = Validation::new(algorithm);
    // Don't enforce exp/nbf/aud automatically — the caller can opt in
    // by validating claims themselves.
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.required_spec_claims.clear();
    let decoded = decode::<serde_json::Value>(&token, &decoding_key, &validation).map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "jwt_verify: {e}"
        ))))
    })?;
    let claims_value = crate::schema::json_to_vm_value(&decoded.claims);
    let mut dict = std::collections::BTreeMap::new();
    dict.insert("valid".to_string(), VmValue::Bool(true));
    dict.insert("claims".to_string(), claims_value);
    Ok(VmValue::Dict(std::sync::Arc::new(dict)))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &BASE64_ENCODE_IMPL_DEF,
    &BASE64_DECODE_IMPL_DEF,
    &BASE64URL_ENCODE_IMPL_DEF,
    &BASE64URL_DECODE_IMPL_DEF,
    &BYTES_TO_BASE64URL_IMPL_DEF,
    &SHA256_BASE64URL_IMPL_DEF,
    &CRYPTO_RANDOM_BYTES_IMPL_DEF,
    &BASE32_ENCODE_IMPL_DEF,
    &BASE32_DECODE_IMPL_DEF,
    &HEX_ENCODE_IMPL_DEF,
    &HEX_DECODE_IMPL_DEF,
    &HASH_VALUE_IMPL_DEF,
    &SHA256_IMPL_DEF,
    &SHA224_IMPL_DEF,
    &SHA384_IMPL_DEF,
    &SHA512_IMPL_DEF,
    &SHA512_256_IMPL_DEF,
    &MD5_IMPL_DEF,
    &SHA256_HEX_IMPL_DEF,
    &HMAC_SHA256_IMPL_DEF,
    &HMAC_SHA256_BASE64_IMPL_DEF,
    &HMAC_SHA1_IMPL_DEF,
    &JWT_SIGN_IMPL_DEF,
    &SIGNED_URL_IMPL_DEF,
    &VERIFY_SIGNED_URL_IMPL_DEF,
    &AWS_SIGV4_HEADERS_IMPL_DEF,
    &CONSTANT_TIME_EQ_IMPL_DEF,
    &URL_ENCODE_IMPL_DEF,
    &URL_DECODE_IMPL_DEF,
    &SHA3_256_IMPL_DEF,
    &SHA3_512_IMPL_DEF,
    &BLAKE3_IMPL_DEF,
    &ED25519_KEYPAIR_IMPL_DEF,
    &ED25519_SIGN_IMPL_DEF,
    &ED25519_VERIFY_IMPL_DEF,
    &X25519_KEYPAIR_IMPL_DEF,
    &X25519_AGREE_IMPL_DEF,
    &JWT_VERIFY_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;
    use std::collections::BTreeMap;

    const ES256_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt\n\
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L\n\
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+\n\
-----END PRIVATE KEY-----\n";

    const ES256_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEw7JAoU/gJbZJvV+zCOvU9yFJq0FN\n\
C/edCMRM78P8eQTBCDUTK1ywSYaszvQZvneiW6gNtWEJndSreEcyyUdVvg==\n\
-----END PUBLIC KEY-----\n";

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_crypto_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(v))
    }

    fn jwt_claims() -> VmValue {
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            ("exp".to_string(), VmValue::Int(4_102_444_800)),
            ("iat".to_string(), VmValue::Int(1_700_000_000)),
            ("iss".to_string(), s("12345")),
        ])))
    }

    fn dict(items: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(std::sync::Arc::new(
            items
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect(),
        ))
    }

    #[test]
    fn base64_round_trip_ascii() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64_encode", vec![s("hello world")]).unwrap();
        assert_eq!(encoded.display(), "aGVsbG8gd29ybGQ=");
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "hello world");
    }

    #[test]
    fn encode_and_hash_bytes_input_is_lossless_past_preview_cap() {
        // Regression: byte-consuming builtins funneled `Bytes` through
        // `display()`, which emits a hex preview truncated at 32 bytes — so the
        // encode/hash of a long binary value silently corrupted it. They must
        // now operate on the raw bytes and agree with the equivalent string.
        let mut vm = vm();
        // 40 bytes (past the 32-byte display preview cap), all printable ASCII
        // so the string and bytes forms carry identical content.
        let raw = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd";
        assert!(raw.len() > 32);
        let bytes_val = VmValue::Bytes(std::sync::Arc::new(raw.as_bytes().to_vec()));
        for name in [
            "base64_encode",
            "base64url_encode",
            "base32_encode",
            "hex_encode",
            "sha256",
            "sha224",
            "sha384",
            "sha512",
            "sha512_256",
            "md5",
            "bytes_to_base64url",
            "sha256_base64url",
        ] {
            let from_bytes = call(&mut vm, name, vec![bytes_val.clone()]).unwrap();
            let from_string = call(&mut vm, name, vec![s(raw)]).unwrap();
            assert_eq!(
                from_bytes.display(),
                from_string.display(),
                "{name}: bytes and string inputs must agree (no truncation)"
            );
        }
        // The encoding must cover all 40 bytes, not a 32-byte prefix.
        let hex = call(&mut vm, "hex_encode", vec![bytes_val]).unwrap();
        assert_eq!(
            hex.display().len(),
            raw.len() * 2,
            "hex must cover all bytes"
        );
    }

    #[test]
    fn base64_empty_string() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64_encode", vec![s("")]).unwrap();
        assert_eq!(encoded.display(), "");
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "");
    }

    #[test]
    fn base64_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base64_decode", vec![s("not-valid-base64!!!")]);
        assert!(result.is_err());
    }

    #[test]
    fn base64_binary_content() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64_encode", vec![s("\x00\x01\x02")]).unwrap();
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "\x00\x01\x02");
    }

    #[test]
    fn base64url_known_vector() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64url_encode", vec![s(">>>???///")]).unwrap();
        assert_eq!(encoded.display(), "Pj4-Pz8_Ly8v");
        let decoded = call(&mut vm, "base64url_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), ">>>???///");
    }

    #[test]
    fn base64url_omits_padding() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64url_encode", vec![s("f")]).unwrap();
        assert_eq!(encoded.display(), "Zg");
    }

    #[test]
    fn base64url_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base64url_decode", vec![s("not+url/safe")]);
        assert!(result.is_err());
    }

    #[test]
    fn base32_known_vector() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base32_encode", vec![s("foobar")]).unwrap();
        assert_eq!(encoded.display(), "MZXW6YTBOI======");
        let decoded = call(&mut vm, "base32_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "foobar");
    }

    #[test]
    fn base32_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base32_decode", vec![s("INVALID-BASE32")]);
        assert!(result.is_err());
    }

    #[test]
    fn hex_round_trip_ascii() {
        let mut vm = vm();
        let encoded = call(&mut vm, "hex_encode", vec![s("hello")]).unwrap();
        assert_eq!(encoded.display(), "68656c6c6f");
        let decoded = call(&mut vm, "hex_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "hello");
    }

    #[test]
    fn hex_round_trip_control_bytes() {
        let mut vm = vm();
        let encoded = call(&mut vm, "hex_encode", vec![s("\x00\x01\x02")]).unwrap();
        assert_eq!(encoded.display(), "000102");
        let decoded = call(&mut vm, "hex_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "\x00\x01\x02");
    }

    #[test]
    fn hex_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "hex_decode", vec![s("abc")]);
        assert!(result.is_err());
    }

    #[test]
    fn base64_encode_accepts_bytes() {
        let mut vm = vm();
        let encoded = call(
            &mut vm,
            "base64_encode",
            vec![VmValue::Bytes(std::sync::Arc::new(vec![0, 1, 2]))],
        )
        .unwrap();
        assert_eq!(encoded.display(), "AAEC");
    }

    #[test]
    fn url_encode_preserves_unreserved() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("hello-world_foo.bar~baz")]).unwrap();
        assert_eq!(result.display(), "hello-world_foo.bar~baz");
    }

    #[test]
    fn url_encode_encodes_special_chars() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("a b&c=d")]).unwrap();
        assert_eq!(result.display(), "a%20b%26c%3Dd");
    }

    #[test]
    fn url_encode_handles_utf8() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("café")]).unwrap();
        assert!(result.display().contains("%C3%A9"));
    }

    #[test]
    fn url_decode_plus_as_space() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("hello+world")]).unwrap();
        assert_eq!(result.display(), "hello world");
    }

    #[test]
    fn url_decode_percent_encoding() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("a%20b%26c")]).unwrap();
        assert_eq!(result.display(), "a b&c");
    }

    #[test]
    fn url_decode_invalid_percent_passthrough() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("100%ZZ")]).unwrap();
        assert_eq!(result.display(), "100%ZZ");
    }

    #[test]
    fn url_round_trip() {
        let mut vm = vm();
        let original = "key=hello world&foo=bar/baz";
        let encoded = call(&mut vm, "url_encode", vec![s(original)]).unwrap();
        let decoded = call(&mut vm, "url_decode", vec![encoded]).unwrap();
        // url_encode emits %20 and url_decode accepts both %20 and +, so the
        // round-trip is exact only as long as encode produces %20.
        assert_eq!(decoded.display(), original);
    }

    #[test]
    fn sha256_known_vector() {
        let mut vm = vm();
        let result = call(&mut vm, "sha256", vec![s("")]).unwrap();
        assert_eq!(
            result.display(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(result.display().len(), 64);
    }

    #[test]
    fn sha224_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha224", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 56);
    }

    #[test]
    fn sha384_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha384", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 96);
    }

    #[test]
    fn sha512_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 128);
    }

    #[test]
    fn sha512_256_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512_256", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 64);
    }

    #[test]
    fn md5_known_vector() {
        let mut vm = vm();
        let result = call(&mut vm, "md5", vec![s("")]).unwrap();
        assert_eq!(result.display(), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn sha256_deterministic() {
        let mut vm = vm();
        let a = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        let b = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        assert_eq!(a.display(), b.display());
    }

    #[test]
    fn sha256_different_inputs_differ() {
        let mut vm = vm();
        let a = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        let b = call(&mut vm, "sha256", vec![s("world")]).unwrap();
        assert_ne!(a.display(), b.display());
    }

    #[test]
    fn hash_value_deterministic() {
        let mut vm = vm();
        let a = call(&mut vm, "hash_value", vec![s("test")]).unwrap();
        let b = call(&mut vm, "hash_value", vec![s("test")]).unwrap();
        assert_eq!(a.display(), b.display());
    }

    #[test]
    fn hash_value_different_inputs() {
        let mut vm = vm();
        let a = call(&mut vm, "hash_value", vec![s("foo")]).unwrap();
        let b = call(&mut vm, "hash_value", vec![s("bar")]).unwrap();
        assert_ne!(a.display(), b.display());
    }

    #[test]
    fn hash_value_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "hash_value", vec![VmValue::Nil]).unwrap();
        assert!(matches!(result, VmValue::Int(_)));
    }

    // RFC 4231 test case 2: key="Jefe", data="what do ya want for nothing?".
    #[test]
    fn hmac_sha256_rfc4231_vector_2() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256",
            vec![s("Jefe"), s("what do ya want for nothing?")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    // GitHub's published HMAC test vector for webhook signature verification.
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
    #[test]
    fn hmac_sha256_github_documented_vector() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256",
            vec![s("It's a Secret to Everybody"), s("Hello, World!")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
    }

    #[test]
    fn hmac_sha256_base64_known_vector() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256_base64",
            vec![s("Jefe"), s("what do ya want for nothing?")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM="
        );
    }

    #[test]
    fn hmac_sha256_empty_inputs() {
        let mut vm = vm();
        let result = call(&mut vm, "hmac_sha256", vec![s(""), s("")]).unwrap();
        assert_eq!(
            result.display(),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad"
        );
    }

    #[test]
    fn signed_url_canonicalizes_query_and_uses_url_safe_signature() {
        let mut vm = vm();
        let signed = call(
            &mut vm,
            "signed_url",
            vec![
                s("https://example.test/receipt/abc?b=two+words&a=1"),
                dict(&[("z", s("slash/value")), ("a2", s("!"))]),
                s("secret"),
                VmValue::Int(1_700_000_600),
            ],
        )
        .unwrap()
        .display();

        assert!(signed.starts_with("https://example.test/receipt/abc?"));
        assert!(signed.contains("a=1"));
        assert!(signed.contains("a2=%21"));
        assert!(signed.contains("b=two%20words"));
        assert!(signed.contains("z=slash%2Fvalue"));
        let sig = signed
            .split('&')
            .find_map(|part| part.strip_prefix("sig="))
            .expect("signature param");
        assert!(!sig.contains('+'));
        assert!(!sig.contains('/'));
        assert!(!sig.contains('='));
    }

    #[test]
    fn verify_signed_url_accepts_valid_path_and_returns_claims() {
        let mut vm = vm();
        let signed = call(
            &mut vm,
            "signed_url",
            vec![
                s("/artifacts/run 1?kind=trace"),
                dict(&[("receipt", s("r_123"))]),
                s("secret"),
                VmValue::Int(200),
            ],
        )
        .unwrap();
        assert!(signed.display().starts_with("/artifacts/run%201?"));
        let verified = call(
            &mut vm,
            "verify_signed_url",
            vec![signed, s("secret"), VmValue::Int(199)],
        )
        .unwrap();
        let VmValue::Dict(result) = verified else {
            panic!("expected verification dict");
        };
        assert!(matches!(result.get("valid"), Some(VmValue::Bool(true))));
        assert!(matches!(
            result.get("signature_valid"),
            Some(VmValue::Bool(true))
        ));
        assert!(matches!(result.get("expired"), Some(VmValue::Bool(false))));
        assert_eq!(result.get("reason").unwrap().display(), "ok");
        let claims = result.get("claims").unwrap().as_dict().unwrap();
        assert_eq!(claims.get("kind").unwrap().display(), "trace");
        assert_eq!(claims.get("receipt").unwrap().display(), "r_123");
    }

    #[test]
    fn verify_signed_url_rejects_tampering() {
        let mut vm = vm();
        let signed = call(
            &mut vm,
            "signed_url",
            vec![
                s("/receipts/r_123"),
                dict(&[("download", s("true"))]),
                s("secret"),
                VmValue::Int(200),
            ],
        )
        .unwrap()
        .display();
        let tampered = signed.replace("download=true", "download=false");
        let verified = call(
            &mut vm,
            "verify_signed_url",
            vec![s(&tampered), s("secret"), VmValue::Int(100)],
        )
        .unwrap();
        let result = verified.as_dict().unwrap();
        assert!(matches!(result.get("valid"), Some(VmValue::Bool(false))));
        assert!(matches!(
            result.get("signature_valid"),
            Some(VmValue::Bool(false))
        ));
        assert_eq!(result.get("reason").unwrap().display(), "bad_signature");
    }

    #[test]
    fn verify_signed_url_handles_expiry_and_skew() {
        let mut vm = vm();
        let signed = call(
            &mut vm,
            "signed_url",
            vec![
                s("/receipts/r_123"),
                dict(&[]),
                s("secret"),
                VmValue::Int(200),
            ],
        )
        .unwrap();
        let expired = call(
            &mut vm,
            "verify_signed_url",
            vec![signed.clone(), s("secret"), VmValue::Int(201)],
        )
        .unwrap();
        let expired_result = expired.as_dict().unwrap();
        assert!(matches!(
            expired_result.get("valid"),
            Some(VmValue::Bool(false))
        ));
        assert!(matches!(
            expired_result.get("signature_valid"),
            Some(VmValue::Bool(true))
        ));
        assert_eq!(expired_result.get("reason").unwrap().display(), "expired");

        let within_skew = call(
            &mut vm,
            "verify_signed_url",
            vec![
                signed,
                s("secret"),
                VmValue::Int(205),
                dict(&[("skew_seconds", VmValue::Int(5))]),
            ],
        )
        .unwrap();
        assert!(matches!(
            within_skew.as_dict().unwrap().get("valid"),
            Some(VmValue::Bool(true))
        ));
    }

    #[test]
    fn signed_url_supports_key_rotation_id() {
        let mut vm = vm();
        let options = dict(&[("kid", s("v2"))]);
        let signed = call(
            &mut vm,
            "signed_url",
            vec![
                s("https://example.test/receipts/r_123"),
                dict(&[("format", s("json"))]),
                s("new-secret"),
                VmValue::Int(200),
                options,
            ],
        )
        .unwrap();
        let keys = dict(&[("v1", s("old-secret")), ("v2", s("new-secret"))]);
        let verified = call(
            &mut vm,
            "verify_signed_url",
            vec![signed, keys, VmValue::Int(100)],
        )
        .unwrap();
        let result = verified.as_dict().unwrap();
        assert!(matches!(result.get("valid"), Some(VmValue::Bool(true))));
        assert_eq!(result.get("kid").unwrap().display(), "v2");
    }

    #[test]
    fn jwt_sign_es256_produces_verifiable_compact_jws() {
        let mut vm = vm();
        let token = call(
            &mut vm,
            "jwt_sign",
            vec![s("ES256"), jwt_claims(), s(ES256_PRIVATE_KEY)],
        )
        .unwrap()
        .display();

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.validate_exp = false;
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            &token,
            &jsonwebtoken::DecodingKey::from_ec_pem(ES256_PUBLIC_KEY.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap();
        assert_eq!(decoded.header.alg, Algorithm::ES256);
        assert_eq!(decoded.claims["iss"], "12345");
        assert_eq!(decoded.claims["iat"], 1_700_000_000);
    }

    #[test]
    fn jwt_sign_rejects_unsupported_algorithm() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "jwt_sign",
            vec![s("HS256"), jwt_claims(), s("secret")],
        );
        let Err(VmError::Runtime(message)) = result else {
            panic!("expected runtime error");
        };
        assert!(message.contains("unsupported algorithm `HS256`"));
    }

    #[test]
    fn jwt_sign_requires_dict_claims() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "jwt_sign",
            vec![s("ES256"), s("not a dict"), s(ES256_PRIVATE_KEY)],
        );
        let Err(VmError::Runtime(message)) = result else {
            panic!("expected runtime error");
        };
        assert!(message.contains("claims must be a dict"));
    }

    #[test]
    fn constant_time_eq_matches_for_equal() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abc")]).unwrap();
        assert!(matches!(result, VmValue::Bool(true)));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abcd")]).unwrap();
        assert!(matches!(result, VmValue::Bool(false)));
    }

    #[test]
    fn constant_time_eq_rejects_different_content() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abd")]).unwrap();
        assert!(matches!(result, VmValue::Bool(false)));
    }
}
