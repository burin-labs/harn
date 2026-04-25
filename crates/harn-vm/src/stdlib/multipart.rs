//! Buffered multipart/form-data helpers for inbound request bodies.

use std::collections::BTreeMap;
use std::rc::Rc;

use sha2::{Digest, Sha256};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_FIELD_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_FIELDS: usize = 128;

#[derive(Debug, Clone)]
struct Limits {
    max_total_bytes: usize,
    max_field_bytes: usize,
    max_fields: usize,
}

#[derive(Debug)]
struct ParsedField {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    headers: BTreeMap<String, String>,
    bytes: Vec<u8>,
}

fn builtin_error(builtin: &str, message: impl std::fmt::Display) -> VmError {
    VmError::Runtime(format!("{builtin}: {message}"))
}

fn string_value(value: impl Into<String>) -> VmValue {
    VmValue::String(Rc::from(value.into()))
}

fn bytes_value(bytes: Vec<u8>) -> VmValue {
    VmValue::Bytes(Rc::new(bytes))
}

fn dict_value(fields: BTreeMap<String, VmValue>) -> VmValue {
    VmValue::Dict(Rc::new(fields))
}

fn list_value(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

fn nil_or_string(value: Option<String>) -> VmValue {
    value.map(string_value).unwrap_or(VmValue::Nil)
}

fn expect_string<'a>(args: &'a [VmValue], index: usize, builtin: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) => Ok(text.as_ref()),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "expected string at argument {}, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("missing argument {}", index + 1),
        )),
    }
}

fn expect_bytes_or_string(
    args: &[VmValue],
    index: usize,
    builtin: &str,
) -> Result<Vec<u8>, VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_ref().clone()),
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "expected bytes or string at argument {}, got {}",
                index + 1,
                other.type_name()
            ),
        )),
        None => Err(builtin_error(
            builtin,
            format!("missing argument {}", index + 1),
        )),
    }
}

fn expect_dict<'a>(
    value: &'a VmValue,
    builtin: &str,
    label: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match value {
        VmValue::Dict(map) => Ok(map.as_ref()),
        other => Err(builtin_error(
            builtin,
            format!("{label} must be a dict, got {}", other.type_name()),
        )),
    }
}

fn optional_options<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
) -> Result<Option<&'a BTreeMap<String, VmValue>>, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(map)) => Ok(Some(map.as_ref())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(builtin_error(
            builtin,
            format!(
                "expected options dict at argument {}, got {}",
                index + 1,
                other.type_name()
            ),
        )),
    }
}

fn opt_usize(
    opts: Option<&BTreeMap<String, VmValue>>,
    key: &str,
    default: usize,
    builtin: &str,
) -> Result<usize, VmError> {
    match opts.and_then(|opts| opts.get(key)) {
        Some(VmValue::Int(value)) if *value >= 0 => Ok(*value as usize),
        Some(VmValue::Int(value)) => Err(builtin_error(
            builtin,
            format!("{key} must be non-negative, got {value}"),
        )),
        Some(other) => Err(builtin_error(
            builtin,
            format!("{key} must be an int, got {}", other.type_name()),
        )),
        None => Ok(default),
    }
}

fn parse_limits(
    opts: Option<&BTreeMap<String, VmValue>>,
    builtin: &str,
) -> Result<Limits, VmError> {
    Ok(Limits {
        max_total_bytes: opt_usize(opts, "max_total_bytes", DEFAULT_MAX_TOTAL_BYTES, builtin)?,
        max_field_bytes: opt_usize(opts, "max_field_bytes", DEFAULT_MAX_FIELD_BYTES, builtin)?,
        max_fields: opt_usize(opts, "max_fields", DEFAULT_MAX_FIELDS, builtin)?,
    })
}

fn split_params(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_quote && ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_quote = !in_quote;
            current.push(ch);
            continue;
        }
        if ch == ';' && !in_quote {
            parts.push(current.trim().to_string());
            current.clear();
            continue;
        }
        current.push(ch);
    }

    parts.push(current.trim().to_string());
    parts
}

fn unquote_param(value: &str) -> String {
    let trimmed = value.trim();
    if !(trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2) {
        return trimmed.to_string();
    }

    let mut out = String::new();
    let mut escaped = false;
    for ch in trimmed[1..trimmed.len() - 1].chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            out.push(ch);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

fn parse_header_value_params(value: &str) -> (String, BTreeMap<String, String>) {
    let mut parts = split_params(value).into_iter();
    let base = parts.next().unwrap_or_default().to_ascii_lowercase();
    let mut params = BTreeMap::new();
    for part in parts {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        params.insert(name, unquote_param(value));
    }
    (base, params)
}

fn validate_boundary(boundary: &str, builtin: &str) -> Result<(), VmError> {
    if boundary.is_empty() {
        return Err(builtin_error(builtin, "boundary must not be empty"));
    }
    if boundary.len() > 70 {
        return Err(builtin_error(builtin, "boundary must be at most 70 bytes"));
    }
    if boundary.bytes().any(|byte| {
        !matches!(
            byte,
            b'0'..=b'9'
                | b'a'..=b'z'
                | b'A'..=b'Z'
                | b'\''
                | b'('
                | b')'
                | b'+'
                | b'_'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'='
                | b'?'
        )
    }) {
        return Err(builtin_error(
            builtin,
            "boundary contains a character that is not valid in multipart/form-data",
        ));
    }
    Ok(())
}

fn boundary_from_content_type(content_type: &str, builtin: &str) -> Result<String, VmError> {
    let (media_type, params) = parse_header_value_params(content_type);
    if media_type != "multipart/form-data" {
        return Err(builtin_error(
            builtin,
            format!("expected multipart/form-data content type, got {media_type}"),
        ));
    }
    let Some(boundary) = params.get("boundary") else {
        return Err(builtin_error(builtin, "content type is missing boundary"));
    };
    validate_boundary(boundary, builtin)?;
    Ok(boundary.clone())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_part_headers(raw: &[u8], builtin: &str) -> Result<BTreeMap<String, String>, VmError> {
    let text = std::str::from_utf8(raw)
        .map_err(|error| builtin_error(builtin, format!("part headers are not UTF-8: {error}")))?;
    let mut headers = BTreeMap::new();
    if text.is_empty() {
        return Ok(headers);
    }

    for line in text.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            return Err(builtin_error(
                builtin,
                format!("malformed part header line `{line}`"),
            ));
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(builtin_error(builtin, "part header name must not be empty"));
        }
        headers.insert(name, value.trim().to_string());
    }
    Ok(headers)
}

fn parse_multipart_body(
    body: &[u8],
    boundary: &str,
    limits: &Limits,
    builtin: &str,
) -> Result<Vec<ParsedField>, VmError> {
    if body.len() > limits.max_total_bytes {
        return Err(builtin_error(
            builtin,
            format!(
                "total body size {} exceeds max_total_bytes {}",
                body.len(),
                limits.max_total_bytes
            ),
        ));
    }

    let delimiter = format!("--{boundary}");
    let delimiter_bytes = delimiter.as_bytes();
    if !body.starts_with(delimiter_bytes) {
        return Err(builtin_error(
            builtin,
            "body does not start with multipart boundary",
        ));
    }

    let mut fields = Vec::new();
    let mut pos = delimiter_bytes.len();

    loop {
        if body.get(pos..pos + 2) == Some(b"--") {
            pos += 2;
            if body.get(pos..pos + 2) == Some(b"\r\n") {
                pos += 2;
            }
            if pos != body.len() {
                return Err(builtin_error(
                    builtin,
                    "unexpected bytes after closing boundary",
                ));
            }
            return Ok(fields);
        }
        if body.get(pos..pos + 2) != Some(b"\r\n") {
            return Err(builtin_error(builtin, "malformed boundary delimiter line"));
        }
        pos += 2;

        if fields.len() >= limits.max_fields {
            return Err(builtin_error(
                builtin,
                format!("field count exceeds max_fields {}", limits.max_fields),
            ));
        }

        let header_rel = find_subslice(&body[pos..], b"\r\n\r\n")
            .ok_or_else(|| builtin_error(builtin, "part is missing header terminator"))?;
        let header_end = pos + header_rel;
        let headers = parse_part_headers(&body[pos..header_end], builtin)?;
        pos = header_end + 4;

        let boundary_marker = format!("\r\n--{boundary}");
        let content_rel = find_subslice(&body[pos..], boundary_marker.as_bytes())
            .ok_or_else(|| builtin_error(builtin, "part is missing following boundary"))?;
        let content_end = pos + content_rel;
        let bytes = body[pos..content_end].to_vec();
        if bytes.len() > limits.max_field_bytes {
            return Err(builtin_error(
                builtin,
                format!(
                    "field size {} exceeds max_field_bytes {}",
                    bytes.len(),
                    limits.max_field_bytes
                ),
            ));
        }

        let Some(disposition) = headers.get("content-disposition") else {
            return Err(builtin_error(
                builtin,
                "part is missing Content-Disposition header",
            ));
        };
        let (disposition_type, params) = parse_header_value_params(disposition);
        if disposition_type != "form-data" {
            return Err(builtin_error(
                builtin,
                format!("expected form-data disposition, got {disposition_type}"),
            ));
        }
        let Some(name) = params.get("name").filter(|name| !name.is_empty()) else {
            return Err(builtin_error(
                builtin,
                "part Content-Disposition is missing name",
            ));
        };
        let content_type = headers.get("content-type").cloned();

        fields.push(ParsedField {
            name: name.clone(),
            filename: params.get("filename").cloned(),
            content_type,
            headers,
            bytes,
        });

        pos = content_end + 2;
        if body.get(pos..pos + delimiter_bytes.len()) != Some(delimiter_bytes) {
            return Err(builtin_error(builtin, "malformed boundary after part"));
        }
        pos += delimiter_bytes.len();
    }
}

fn headers_value(headers: &BTreeMap<String, String>) -> VmValue {
    let map = headers
        .iter()
        .map(|(key, value)| (key.clone(), string_value(value)))
        .collect();
    dict_value(map)
}

fn parsed_field_value(field: ParsedField) -> VmValue {
    let text = match std::str::from_utf8(&field.bytes) {
        Ok(text) => string_value(text),
        Err(_) => VmValue::Nil,
    };
    let mut map = BTreeMap::new();
    map.insert("name".to_string(), string_value(field.name));
    map.insert("filename".to_string(), nil_or_string(field.filename));
    map.insert(
        "content_type".to_string(),
        nil_or_string(field.content_type),
    );
    map.insert("headers".to_string(), headers_value(&field.headers));
    map.insert("bytes".to_string(), bytes_value(field.bytes));
    map.insert("text".to_string(), text);
    dict_value(map)
}

fn multipart_parse_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let body = expect_bytes_or_string(args, 0, "multipart_parse")?;
    let content_type = expect_string(args, 1, "multipart_parse")?;
    let opts = optional_options(args, 2, "multipart_parse")?;
    let limits = parse_limits(opts, "multipart_parse")?;
    let boundary = boundary_from_content_type(content_type, "multipart_parse")?;
    let total_bytes = body.len() as i64;
    let fields = parse_multipart_body(&body, &boundary, &limits, "multipart_parse")?;
    let field_count = fields.len() as i64;

    let mut result = BTreeMap::new();
    result.insert("boundary".to_string(), string_value(boundary));
    result.insert(
        "fields".to_string(),
        list_value(fields.into_iter().map(parsed_field_value).collect()),
    );
    result.insert("field_count".to_string(), VmValue::Int(field_count));
    result.insert("total_bytes".to_string(), VmValue::Int(total_bytes));
    Ok(dict_value(result))
}

fn field_dict<'a>(
    args: &'a [VmValue],
    builtin: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match args.first() {
        Some(value) => expect_dict(value, builtin, "field"),
        None => Err(builtin_error(builtin, "missing argument 1")),
    }
}

fn multipart_field_bytes_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let field = field_dict(args, "multipart_field_bytes")?;
    match field.get("bytes") {
        Some(VmValue::Bytes(bytes)) => Ok(VmValue::Bytes(bytes.clone())),
        Some(other) => Err(builtin_error(
            "multipart_field_bytes",
            format!("field.bytes must be bytes, got {}", other.type_name()),
        )),
        None => Err(builtin_error(
            "multipart_field_bytes",
            "field is missing bytes",
        )),
    }
}

fn multipart_field_text_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let field = field_dict(args, "multipart_field_text")?;
    let bytes = match field.get("bytes") {
        Some(VmValue::Bytes(bytes)) => bytes.as_slice(),
        Some(other) => {
            return Err(builtin_error(
                "multipart_field_text",
                format!("field.bytes must be bytes, got {}", other.type_name()),
            ));
        }
        None => {
            return Err(builtin_error(
                "multipart_field_text",
                "field is missing bytes",
            ));
        }
    };
    let text =
        std::str::from_utf8(bytes).map_err(|error| builtin_error("multipart_field_text", error))?;
    Ok(string_value(text))
}

fn field_input_string(
    field: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
    required: bool,
) -> Result<Option<String>, VmError> {
    match field.get(key) {
        Some(VmValue::String(value)) if !required || !value.is_empty() => {
            Ok(Some(value.to_string()))
        }
        Some(VmValue::String(_)) => Err(builtin_error(builtin, format!("{key} must not be empty"))),
        Some(VmValue::Nil) | None if !required => Ok(None),
        None => Err(builtin_error(builtin, format!("field is missing {key}"))),
        Some(other) => Err(builtin_error(
            builtin,
            format!("{key} must be a string, got {}", other.type_name()),
        )),
    }
}

fn field_input_content(
    field: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<Vec<u8>, VmError> {
    match field.get("content").or_else(|| field.get("value")) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_ref().clone()),
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(other) => Err(builtin_error(
            builtin,
            format!("content must be bytes or string, got {}", other.type_name()),
        )),
        None => Err(builtin_error(builtin, "field is missing content")),
    }
}

fn input_headers(
    field: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<BTreeMap<String, String>, VmError> {
    let Some(value) = field.get("headers") else {
        return Ok(BTreeMap::new());
    };
    let headers = expect_dict(value, builtin, "headers")?;
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        if name.contains('\r') || name.contains('\n') || name.trim().is_empty() {
            return Err(builtin_error(builtin, "header names must be single-line"));
        }
        let lower = name.to_ascii_lowercase();
        if matches!(lower.as_str(), "content-disposition" | "content-type") {
            return Err(builtin_error(
                builtin,
                format!("{name} is managed by multipart_form_data"),
            ));
        }
        let VmValue::String(value) = value else {
            return Err(builtin_error(
                builtin,
                format!("header {name} must be a string, got {}", value.type_name()),
            ));
        };
        if value.contains('\r') || value.contains('\n') {
            return Err(builtin_error(builtin, "header values must be single-line"));
        }
        out.insert(name.clone(), value.to_string());
    }
    Ok(out)
}

fn quote_header_param(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn write_field_part(
    out: &mut Vec<u8>,
    boundary: &str,
    field: &BTreeMap<String, VmValue>,
) -> Result<(), VmError> {
    let name = field_input_string(field, "name", "multipart_form_data", true)?.unwrap();
    if name.contains('\r') || name.contains('\n') {
        return Err(builtin_error(
            "multipart_form_data",
            "field name must be single-line",
        ));
    }
    let filename = field_input_string(field, "filename", "multipart_form_data", false)?;
    let content_type = field_input_string(field, "content_type", "multipart_form_data", false)?;
    let content = field_input_content(field, "multipart_form_data")?;
    let headers = input_headers(field, "multipart_form_data")?;

    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name={}",
            quote_header_param(&name)
        )
        .as_bytes(),
    );
    if let Some(filename) = filename {
        if filename.contains('\r') || filename.contains('\n') {
            return Err(builtin_error(
                "multipart_form_data",
                "filename must be single-line",
            ));
        }
        out.extend_from_slice(format!("; filename={}", quote_header_param(&filename)).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    if let Some(content_type) = content_type {
        if content_type.contains('\r') || content_type.contains('\n') {
            return Err(builtin_error(
                "multipart_form_data",
                "content_type must be single-line",
            ));
        }
        out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&content);
    out.extend_from_slice(b"\r\n");
    Ok(())
}

fn build_body(fields: &[VmValue], boundary: &str) -> Result<Vec<u8>, VmError> {
    validate_boundary(boundary, "multipart_form_data")?;
    let mut out = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let field = expect_dict(
            field,
            "multipart_form_data",
            &format!("field {}", index + 1),
        )?;
        write_field_part(&mut out, boundary, field)?;
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(out)
}

fn deterministic_boundary(fields: &[VmValue]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update(field.display().as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize();
    format!("----harn-boundary-{}", hex::encode(&digest[..8]))
}

fn field_content_contains_boundary(field: &VmValue, boundary: &str) -> bool {
    let value = match field {
        VmValue::Dict(map) => map.get("content").or_else(|| map.get("value")),
        VmValue::StructInstance { .. } => field
            .struct_field("content")
            .or_else(|| field.struct_field("value")),
        _ => None,
    };

    value.is_some_and(|value| match value {
        VmValue::Bytes(bytes) => find_subslice(bytes, boundary.as_bytes()).is_some(),
        VmValue::String(text) => text.contains(boundary),
        _ => false,
    })
}

fn multipart_form_data_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    let fields = match args.first() {
        Some(VmValue::List(fields)) => fields.as_ref(),
        Some(other) => {
            return Err(builtin_error(
                "multipart_form_data",
                format!("fields must be a list, got {}", other.type_name()),
            ));
        }
        None => return Err(builtin_error("multipart_form_data", "missing argument 1")),
    };
    let opts = optional_options(args, 1, "multipart_form_data")?;
    if let Some(boundary_value) = opts.and_then(|opts| opts.get("boundary")) {
        let boundary = match boundary_value {
            VmValue::String(boundary) => boundary.to_string(),
            VmValue::Nil => deterministic_boundary(fields),
            other => {
                return Err(builtin_error(
                    "multipart_form_data",
                    format!("boundary must be a string, got {}", other.type_name()),
                ));
            }
        };
        let body = build_body(fields, &boundary)?;
        return form_data_result(boundary, body);
    }

    let boundary = deterministic_boundary(fields);
    for counter in 0..100 {
        let candidate = if counter == 0 {
            boundary.clone()
        } else {
            format!("{boundary}-{counter}")
        };
        if fields
            .iter()
            .any(|field| field_content_contains_boundary(field, &candidate))
        {
            continue;
        }
        let body = build_body(fields, &candidate)?;
        return form_data_result(candidate, body);
    }

    Err(builtin_error(
        "multipart_form_data",
        "could not generate a boundary absent from field content",
    ))
}

fn form_data_result(boundary: String, body: Vec<u8>) -> Result<VmValue, VmError> {
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let mut result = BTreeMap::new();
    result.insert("boundary".to_string(), string_value(boundary));
    result.insert("content_type".to_string(), string_value(content_type));
    result.insert("body".to_string(), bytes_value(body));
    Ok(dict_value(result))
}

pub(crate) fn register_multipart_builtins(vm: &mut Vm) {
    vm.register_builtin("multipart_parse", |args, _out| {
        multipart_parse_builtin(args)
    });
    vm.register_builtin("multipart_field_bytes", |args, _out| {
        multipart_field_bytes_builtin(args)
    });
    vm.register_builtin("multipart_field_text", |args, _out| {
        multipart_field_text_builtin(args)
    });
    vm.register_builtin("multipart_form_data", |args, _out| {
        multipart_form_data_builtin(args)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> VmValue {
        string_value(value)
    }

    fn b(value: &[u8]) -> VmValue {
        bytes_value(value.to_vec())
    }

    fn field(entries: &[(&str, VmValue)]) -> VmValue {
        dict_value(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        )
    }

    fn call(name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let mut vm = Vm::new();
        register_multipart_builtins(&mut vm);
        let func = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        func(&args, &mut out)
    }

    #[test]
    fn parses_text_and_file_fields() {
        let built = call(
            "multipart_form_data",
            vec![list_value(vec![
                field(&[("name", s("title")), ("content", s("Hello"))]),
                field(&[
                    ("name", s("upload")),
                    ("filename", s("a.bin")),
                    ("content_type", s("application/octet-stream")),
                    ("content", b(&[0, 1, 255])),
                ]),
            ])],
        )
        .unwrap();
        let built = expect_dict(&built, "test", "built").unwrap();
        let parsed = call(
            "multipart_parse",
            vec![
                built.get("body").unwrap().clone(),
                built.get("content_type").unwrap().clone(),
            ],
        )
        .unwrap();
        let parsed = expect_dict(&parsed, "test", "parsed").unwrap();
        assert!(matches!(parsed.get("field_count"), Some(VmValue::Int(2))));
    }

    #[test]
    fn rejects_field_limit() {
        let body = b(b"--x\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--x--\r\n");
        let err = call(
            "multipart_parse",
            vec![
                body,
                s("multipart/form-data; boundary=x"),
                dict_value(BTreeMap::from([(
                    "max_fields".to_string(),
                    VmValue::Int(0),
                )])),
            ],
        )
        .unwrap_err();
        assert!(format!("{err}").contains("max_fields"));
    }
}
