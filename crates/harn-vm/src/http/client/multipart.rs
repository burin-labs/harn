use base64::Engine;

use super::{resolve_http_path, value_to_bytes, vm_error, MULTIPART_MOCK_BOUNDARY};
use crate::value::{VmError, VmValue};

#[derive(Clone)]
pub(super) struct MultipartRequest {
    parts: Vec<MultipartField>,
    pub(super) mock_body: String,
}

#[derive(Clone)]
struct MultipartField {
    name: String,
    value: Vec<u8>,
    filename: Option<String>,
    content_type: Option<String>,
    headers: reqwest::header::HeaderMap,
}

fn parse_multipart_field(value: &VmValue) -> Result<MultipartField, VmError> {
    let dict = value
        .as_dict()
        .ok_or_else(|| vm_error("http: multipart entries must be dicts"))?;
    let name = dict
        .get("name")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| vm_error("http: multipart entry requires name"))?;
    let content_type = dict
        .get("content_type")
        .or_else(|| dict.get("mime_type"))
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(value) = dict.get("headers") {
        let values = value
            .as_dict()
            .ok_or_else(|| vm_error("http: multipart headers must be a dict"))?;
        for (name, value) in values {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| vm_error("http: invalid multipart header name"))?;
            if name == reqwest::header::CONTENT_TYPE || name == reqwest::header::CONTENT_DISPOSITION
            {
                return Err(vm_error("http: multipart Content-Type and Content-Disposition use content_type, name and filename"));
            }
            if headers.contains_key(&name) {
                return Err(vm_error("http: duplicate multipart header name"));
            }
            let VmValue::String(value) = value else {
                return Err(vm_error("http: multipart header values must be strings"));
            };
            let value = reqwest::header::HeaderValue::from_str(value.as_str())
                .map_err(|_| vm_error("http: invalid multipart header value"))?;
            headers.insert(name, value);
        }
    }

    let mut filename = dict
        .get("filename")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let value = if let Some(path_value) = dict.get("path") {
        let path = path_value.display();
        let resolved = resolve_http_path(
            "http multipart",
            &path,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        if filename.is_none() {
            filename = resolved
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string());
        }
        std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read multipart file {}: {error}",
                resolved.display()
            ))
        })?
    } else if let Some(base64_value) = dict.get("value_base64").or_else(|| dict.get("base64")) {
        base64::engine::general_purpose::STANDARD
            .decode(base64_value.display())
            .map_err(|error| vm_error(format!("http: invalid multipart base64 value: {error}")))?
    } else {
        dict.get("value").map(value_to_bytes).ok_or_else(|| {
            vm_error("http: multipart entry requires value, value_base64, or path")
        })?
    };

    Ok(MultipartField {
        name,
        value,
        filename,
        content_type,
        headers,
    })
}

pub(super) fn parse_multipart_request(
    options: &crate::value::DictMap,
) -> Result<Option<MultipartRequest>, VmError> {
    let Some(value) = options.get("multipart") else {
        return Ok(None);
    };
    let VmValue::List(items) = value else {
        return Err(vm_error("http: multipart must be a list"));
    };
    let parts = items
        .iter()
        .map(parse_multipart_field)
        .collect::<Result<Vec<_>, _>>()?;
    let mock_body = multipart_mock_body(&parts);
    Ok(Some(MultipartRequest { parts, mock_body }))
}

fn multipart_mock_body(parts: &[MultipartField]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str("--");
        out.push_str(MULTIPART_MOCK_BOUNDARY);
        out.push_str("\r\nContent-Disposition: form-data; name=\"");
        out.push_str(&part.name);
        out.push('"');
        if let Some(filename) = &part.filename {
            out.push_str("; filename=\"");
            out.push_str(filename);
            out.push('"');
        }
        out.push_str("\r\n");
        if let Some(content_type) = &part.content_type {
            out.push_str("Content-Type: ");
            out.push_str(content_type);
            out.push_str("\r\n");
        }
        for (name, value) in &part.headers {
            out.push_str(name.as_str());
            out.push_str(": ");
            out.push_str(&String::from_utf8_lossy(value.as_bytes()));
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&String::from_utf8_lossy(&part.value));
        out.push_str("\r\n");
    }
    out.push_str("--");
    out.push_str(MULTIPART_MOCK_BOUNDARY);
    out.push_str("--\r\n");
    out
}

pub(super) fn multipart_form(
    request: &MultipartRequest,
) -> Result<reqwest::multipart::Form, VmError> {
    let mut form = reqwest::multipart::Form::new();
    for field in &request.parts {
        let mut part = reqwest::multipart::Part::bytes(field.value.clone());
        if let Some(filename) = &field.filename {
            part = part.file_name(filename.clone());
        }
        if let Some(content_type) = &field.content_type {
            part = part.mime_str(content_type).map_err(|error| {
                vm_error(format!("http: invalid multipart content_type: {error}"))
            })?;
        }
        form = form.part(field.name.clone(), part.headers(field.headers.clone()));
    }
    Ok(form)
}
