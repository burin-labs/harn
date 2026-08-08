use std::collections::BTreeMap;

use chrono::Utc;

use super::client::HttpRequestParts;
use super::{string_option, vm_error};
use crate::value::{VmDictExt, VmError, VmValue};

pub(super) async fn apply(
    options: &crate::value::DictMap,
    final_url: &str,
    parts: &mut HttpRequestParts,
) -> Result<(), VmError> {
    let Some(value) = options.get("aws_sigv4") else {
        return Ok(());
    };
    let VmValue::Dict(spec) = value else {
        return Err(vm_error("http: aws_sigv4 must be a dict"));
    };
    let service = string_option(spec, "service")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| vm_error("http: aws_sigv4.service is required"))?;
    let region_override = string_option(spec, "region");
    let region =
        crate::llm::providers::bedrock::resolve_live_region(region_override.as_deref()).await?;
    let credentials =
        crate::llm::providers::bedrock::resolve_aws_credentials(region.as_str()).await?;
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_string(), value.to_string()))
                .map_err(|_| vm_error(format!("http: header {} is not valid text", name.as_str())))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let signed = crate::aws_sigv4::sign(crate::aws_sigv4::AwsSigV4Input {
        credentials: &credentials,
        method: parts.method.as_str(),
        url: final_url,
        service: service.as_str(),
        region: region.as_str(),
        headers: &headers,
        body: parts.body.as_deref().unwrap_or_default().as_bytes(),
        timestamp: Utc::now(),
    })
    .map_err(|error| vm_error(format!("http: aws_sigv4 signing failed: {error}")))?;

    let mut signed_headers = reqwest::header::HeaderMap::new();
    let mut recorded_headers = crate::value::DictMap::new();
    for (name, value) in signed.headers {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| vm_error(format!("http: signer returned invalid header name {name}")))?;
        let header_value = reqwest::header::HeaderValue::from_str(value.as_str())
            .map_err(|_| vm_error(format!("http: signer returned invalid header {name}")))?;
        recorded_headers.put_str(header_name.as_str(), value.as_str());
        signed_headers.insert(header_name, header_value);
    }
    parts.headers = signed_headers;
    parts.recorded_headers = recorded_headers;
    Ok(())
}
