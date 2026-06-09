//! ACP authentication metadata normalization.
use super::*;

pub(super) fn harn_auth_meta(
    params: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    params
        .get("_meta")
        .and_then(|value| value.get("harn"))
        .and_then(|value| value.as_object())
}

pub(super) fn harn_auth_string<'a>(
    meta: &'a serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| meta.get(*field).and_then(|value| value.as_str()))
        .or_else(|| {
            let credentials = meta.get("credentials")?.as_object()?;
            fields
                .iter()
                .find_map(|field| credentials.get(*field).and_then(|value| value.as_str()))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn harn_auth_headers(
    meta: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, String> {
    let Some(headers) = meta.get("headers").and_then(|value| value.as_object()) else {
        return BTreeMap::new();
    };
    headers
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect()
}

pub(super) fn acp_auth_request_for_method(
    method: &AuthMethodConfig,
    params: &serde_json::Value,
) -> Result<AuthRequest, String> {
    let meta = harn_auth_meta(params).ok_or_else(|| {
        "authenticate requires `_meta.harn` credentials for Harn auth policies".to_string()
    })?;
    let mut request = AuthRequest {
        method: harn_auth_string(meta, &["method"])
            .unwrap_or("ACP")
            .to_string(),
        path: harn_auth_string(meta, &["path"])
            .unwrap_or("authenticate")
            .to_string(),
        body: harn_auth_string(meta, &["body"])
            .map(|value| value.as_bytes().to_vec())
            .unwrap_or_default(),
        headers: harn_auth_headers(meta),
        ..AuthRequest::default()
    };

    match method {
        AuthMethodConfig::ApiKey(_) => {
            if request.headers.is_empty() {
                let api_key =
                    harn_auth_string(meta, &["apiKey", "api_key", "token", "bearerToken"])
                        .ok_or_else(|| {
                            "authenticate requires an API key in `_meta.harn.apiKey`".to_string()
                        })?;
                request
                    .headers
                    .insert("x-api-key".to_string(), api_key.to_string());
            }
        }
        AuthMethodConfig::Hmac(_) => {
            if request.headers.is_empty() {
                return Err(
                    "authenticate requires HMAC headers in `_meta.harn.headers`".to_string()
                );
            }
        }
        AuthMethodConfig::OAuth21(_) => {
            return Err(
                "OAuth ACP authentication requires transport-validated bearer claims".to_string(),
            );
        }
    }

    Ok(request)
}
