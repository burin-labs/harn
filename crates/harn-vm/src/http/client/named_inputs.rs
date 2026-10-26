use super::*;

pub(in crate::http) fn build_http_download_response(
    status: i64,
    headers: crate::value::DictMap,
    bytes_written: u64,
) -> VmValue {
    let mut result = crate::value::DictMap::new();
    result.put_int("status", status);
    result.put("headers", VmValue::dict(headers));
    result.put_int("bytes_written", u64_to_vm_int(bytes_written));
    result.put_bool("ok", (200..300).contains(&(status as u16)));
    VmValue::dict(result)
}

pub(in crate::http) fn build_http_response(
    status: i64,
    headers: crate::value::DictMap,
    body: impl Into<super::ResponseBody>,
    final_url: &str,
    private_filter: &crate::session_inputs::OutputFilter,
) -> VmValue {
    let mut result = crate::value::DictMap::new();
    result.put_int("status", status);
    let headers: crate::value::DictMap = headers
        .into_iter()
        .map(|(name, value)| {
            (
                crate::value::intern_key(&private_filter.redact(name.as_str())),
                VmValue::string(private_filter.redact(&value.display())),
            )
        })
        .collect();
    result.put("headers", VmValue::dict(headers));
    match body.into() {
        super::ResponseBody::Text(body) => result.put_str("body", private_filter.redact(&body)),
        super::ResponseBody::Bytes(body) => {
            result.put_str("body", "");
            result.put(
                "body_bytes",
                VmValue::Bytes(Arc::new(private_filter.redact_bytes(&body).into_owned())),
            );
        }
    }
    result.put_str("final_url", private_filter.redact(final_url));
    result.put_bool("ok", (200..300).contains(&(status as u16)));
    VmValue::dict(result)
}

pub(super) fn apply_session_secret_headers(
    options: &crate::value::DictMap,
    url: &str,
    parts: &mut HttpRequestParts,
) -> Result<(), VmError> {
    let Some(raw) = options.get("secret_headers") else {
        return Ok(());
    };
    let bindings = raw
        .as_dict()
        .ok_or_else(|| vm_error("secret_headers must map header names to named inputs"))?;
    if parts.headers.contains_key(reqwest::header::HOST) {
        return Err(vm_error(
            "Secret headers cannot be combined with a custom Host header",
        ));
    }
    let destination = url::Url::parse(url)
        .map_err(|_| vm_error("A valid HTTPS destination is required for secret headers"))?;
    if destination.scheme() != "https"
        || destination.host_str().is_none()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || bindings.len() > 32
    {
        return Err(vm_error("Secret headers require an HTTPS destination without URL credentials and at most 32 bindings"));
    }
    let usage = if ["proxy", "proxy_auth", "no_proxy", "tls"]
        .iter()
        .any(|key| options.contains_key(*key))
    {
        crate::session_inputs::InputUse::HttpWithCustomTransport(url)
    } else {
        crate::session_inputs::InputUse::Http(url)
    };
    for (header, binding) in bindings {
        let header_name = reqwest::header::HeaderName::from_bytes(header.as_bytes())
            .map_err(|_| vm_error("A secret header name is invalid"))?;
        if header_name == reqwest::header::HOST {
            return Err(vm_error("A named input cannot supply the Host header"));
        }
        if parts.headers.contains_key(&header_name) {
            return Err(vm_error(
                "A secret header cannot replace an ordinary header",
            ));
        }
        let binding = binding
            .as_dict()
            .ok_or_else(|| vm_error("A secret header binding requires name and optional prefix"))?;
        if binding.keys().any(|key| key != "name" && key != "prefix") {
            return Err(vm_error(
                "A secret header binding accepts only name and prefix",
            ));
        }
        let Some(VmValue::String(name)) = binding.get("name") else {
            return Err(vm_error("A secret header binding requires an input name"));
        };
        let prefix = match binding.get("prefix") {
            None => "",
            Some(VmValue::String(value)) => value.as_ref(),
            _ => return Err(vm_error("A secret header prefix must be a string")),
        };
        let value = zeroize::Zeroizing::new(
            crate::session_inputs::http_header(name.as_str(), usage, prefix)
                .map_err(|error| vm_error(error.to_string()))?,
        );
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| vm_error("The named input cannot be used as an HTTP header"))?;
        value.set_sensitive(true);
        parts.headers.insert(header_name, value);
        parts
            .recorded_headers
            .put_str(header.as_str(), "[redacted]");
    }
    Ok(())
}
