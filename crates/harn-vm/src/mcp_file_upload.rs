//! Experimental MCP file-input support.
//!
//! SPECULATIVE: implements MCP file upload per draft SEP-2356:
//! https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2356.
//! This should be revisited when the MCP proposal is ratified.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base64::Engine;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub const SPEC_REVISION: &str =
    "modelcontextprotocol/modelcontextprotocol#2356@ff9d03efb9e160481d537985128a0fd28a157778";
pub const SPEC_URL: &str = "https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2356";
const X_MCP_FILE: &str = "x-mcp-file";

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileUploadConfig {
    spec_revision: String,
}

thread_local! {
    static FILE_UPLOAD_CONFIG: RefCell<Option<FileUploadConfig>> = const { RefCell::new(None) };
}

pub fn is_enabled() -> bool {
    FILE_UPLOAD_CONFIG.with(|cell| cell.borrow().is_some())
}

#[cfg(any(test, feature = "vm-bench-internals"))]
pub fn reset_for_tests() {
    FILE_UPLOAD_CONFIG.with(|cell| *cell.borrow_mut() = None);
}

pub fn register_mcp_file_upload_builtins(vm: &mut Vm) {
    vm.register_builtin("mcp_configure", |args, _out| {
        configure(args.first().unwrap_or(&VmValue::Nil))
    });
    vm.register_builtin("harn.mcp.configure", |args, _out| {
        configure(args.first().unwrap_or(&VmValue::Nil))
    });

    vm.register_builtin("mcp_file_input", |args, _out| {
        file_input_schema(args.first().unwrap_or(&VmValue::Nil))
    });
    vm.register_builtin("harn.mcp.file_input", |args, _out| {
        file_input_schema(args.first().unwrap_or(&VmValue::Nil))
    });

    vm.register_async_builtin(
        "mcp_upload_file",
        |_ctx, args| async move { upload_file(&args) },
    );
    vm.register_async_builtin("harn.mcp.upload_file", |_ctx, args| async move {
        upload_file(&args)
    });
}

fn configure(config: &VmValue) -> Result<VmValue, VmError> {
    let file_upload = match config {
        VmValue::Nil => None,
        VmValue::Dict(root) => root
            .get("experimental")
            .and_then(VmValue::as_dict)
            .and_then(|experimental| experimental.get("file_upload")),
        other => {
            return Err(VmError::Runtime(format!(
                "mcp.configure: config must be a dict, got {}",
                other.type_name()
            )));
        }
    };

    match file_upload {
        Some(VmValue::Dict(options)) => {
            let enabled = optional_bool(options, "enabled").unwrap_or(true);
            if !enabled {
                FILE_UPLOAD_CONFIG.with(|cell| *cell.borrow_mut() = None);
                return Ok(status_value());
            }
            let requested = string_field(options, "spec_revision").ok_or_else(|| {
                VmError::Runtime(
                    "mcp.configure: experimental.file_upload.spec_revision is required".into(),
                )
            })?;
            if !revision_matches_supported(&requested) {
                return Err(VmError::Runtime(format!(
                    "mcp.configure: unsupported experimental.file_upload.spec_revision \
                     {requested:?}; this build implements {SPEC_REVISION}"
                )));
            }
            FILE_UPLOAD_CONFIG.with(|cell| {
                *cell.borrow_mut() = Some(FileUploadConfig {
                    spec_revision: SPEC_REVISION.to_string(),
                });
            });
            Ok(status_value())
        }
        Some(VmValue::Bool(false)) | Some(VmValue::Nil) | None => {
            FILE_UPLOAD_CONFIG.with(|cell| *cell.borrow_mut() = None);
            Ok(status_value())
        }
        Some(other) => Err(VmError::Runtime(format!(
            "mcp.configure: experimental.file_upload must be a dict or false, got {}",
            other.type_name()
        ))),
    }
}

fn revision_matches_supported(requested: &str) -> bool {
    matches!(
        requested,
        SPEC_REVISION
            | "modelcontextprotocol/modelcontextprotocol#2356"
            | "modelcontextprotocol#2356"
            | "#2356"
            | "2356"
            | "ff9d03efb9e160481d537985128a0fd28a157778"
    )
}

fn status_value() -> VmValue {
    let config = FILE_UPLOAD_CONFIG.with(|cell| cell.borrow().clone());
    let enabled = config.is_some();
    let mut file_upload = BTreeMap::new();
    file_upload.insert("enabled".to_string(), VmValue::Bool(enabled));
    file_upload.insert(
        "spec_revision".to_string(),
        if let Some(config) = config {
            VmValue::String(Rc::from(config.spec_revision))
        } else {
            VmValue::Nil
        },
    );
    file_upload.insert(
        "proposal_url".to_string(),
        VmValue::String(Rc::from(SPEC_URL)),
    );
    file_upload.insert(
        "wire_format".to_string(),
        VmValue::String(Rc::from("x-mcp-file+rfc2397-data-uri")),
    );

    let mut experimental = BTreeMap::new();
    experimental.insert(
        "file_upload".to_string(),
        VmValue::Dict(Rc::new(file_upload)),
    );

    let mut root = BTreeMap::new();
    root.insert(
        "experimental".to_string(),
        VmValue::Dict(Rc::new(experimental)),
    );
    VmValue::Dict(Rc::new(root))
}

fn ensure_enabled(name: &str) -> Result<(), VmError> {
    if is_enabled() {
        return Ok(());
    }
    Err(VmError::Runtime(format!(
        "{name}: experimental MCP file upload is disabled; call \
         mcp.configure({{experimental: {{file_upload: {{spec_revision: \"{SPEC_REVISION}\"}}}}}}) \
         before using draft MCP SEP-2356 file inputs"
    )))
}

fn file_input_schema(options: &VmValue) -> Result<VmValue, VmError> {
    ensure_enabled("mcp.file_input")?;
    let options = match options {
        VmValue::Nil => None,
        VmValue::Dict(options) => Some(options.as_ref()),
        other => {
            return Err(VmError::Runtime(format!(
                "mcp.file_input: options must be a dict, got {}",
                other.type_name()
            )));
        }
    };

    let mut descriptor = BTreeMap::new();
    if let Some(accept) = match options {
        Some(opts) => list_of_strings(opts.get("accept"), "accept", "mcp.file_input")?,
        None => None,
    } {
        descriptor.insert("accept".to_string(), VmValue::List(Rc::new(accept)));
    }
    if let Some(max_size) =
        options.and_then(|opts| int_field(opts, "max_size").or_else(|| int_field(opts, "maxSize")))
    {
        if max_size < 0 {
            return Err(VmError::Runtime(
                "mcp.file_input: max_size must be non-negative".into(),
            ));
        }
        descriptor.insert("maxSize".to_string(), VmValue::Int(max_size));
    }

    let mut schema = BTreeMap::new();
    schema.insert("type".to_string(), VmValue::String(Rc::from("string")));
    schema.insert("format".to_string(), VmValue::String(Rc::from("uri")));
    schema.insert(X_MCP_FILE.to_string(), VmValue::Dict(Rc::new(descriptor)));
    if let Some(title) = options.and_then(|opts| string_field(opts, "title")) {
        schema.insert("title".to_string(), VmValue::String(Rc::from(title)));
    }
    if let Some(description) = options.and_then(|opts| string_field(opts, "description")) {
        schema.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description)),
        );
    }
    Ok(VmValue::Dict(Rc::new(schema)))
}

fn upload_file(args: &[VmValue]) -> Result<VmValue, VmError> {
    ensure_enabled("mcp.upload_file")?;
    if args.is_empty() {
        return Err(VmError::Runtime(
            "mcp.upload_file: server/client argument is required".into(),
        ));
    }
    let path = match args.get(1) {
        Some(VmValue::String(path)) => path.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: file_path must be a string, got {}",
                other.type_name()
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "mcp.upload_file: file_path argument is required".into(),
            ));
        }
    };
    let options = match args.get(2) {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Dict(options)) => Some(options.as_ref()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: options must be a dict, got {}",
                other.type_name()
            )));
        }
    };

    let resolved_path = resolve_upload_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "mcp.upload_file",
        &resolved_path,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    // Validate the file size BEFORE reading the contents. The previous
    // ordering read the whole file into memory and only then compared
    // against `max_size`, which made the size cap useless against a
    // ~unbounded file (the OOM/oversized-read fired before the check).
    let max_size =
        options.and_then(|opts| int_field(opts, "max_size").or_else(|| int_field(opts, "maxSize")));
    if let Some(max_size) = max_size {
        if max_size < 0 {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: maxSize must be non-negative, got {max_size}",
            )));
        }
        let metadata = std::fs::metadata(&resolved_path).map_err(|error| {
            VmError::Runtime(format!(
                "mcp.upload_file: failed to stat {}: {error}",
                resolved_path.display()
            ))
        })?;
        if metadata.len() > max_size as u64 {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: {} is {} bytes, exceeding maxSize {}",
                resolved_path.display(),
                metadata.len(),
                max_size
            )));
        }
    }
    let bytes = std::fs::read(&resolved_path).map_err(|error| {
        VmError::Runtime(format!(
            "mcp.upload_file: failed to read {}: {error}",
            resolved_path.display()
        ))
    })?;
    // Defense-in-depth: re-check after the read in case the file grew
    // between the stat and the read.
    if let Some(max_size) = max_size {
        if bytes.len() as u64 > max_size as u64 {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: {} grew past maxSize ({} > {}) between stat and read",
                resolved_path.display(),
                bytes.len(),
                max_size
            )));
        }
    }

    let media_type = options
        .and_then(|opts| {
            string_field(opts, "media_type").or_else(|| string_field(opts, "mime_type"))
        })
        .unwrap_or_else(|| infer_media_type(&resolved_path));
    validate_media_type(&media_type)?;

    if let Some(accept) = match options {
        Some(opts) => string_vec_field(opts, "accept", "mcp.upload_file")?,
        None => None,
    } {
        let accept_refs = accept.iter().map(String::as_str).collect::<Vec<_>>();
        if !media_type_matches_accept(&media_type, &accept_refs) {
            return Err(VmError::Runtime(format!(
                "mcp.upload_file: media type {media_type:?} does not match accept {accept:?}"
            )));
        }
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(VmValue::String(Rc::from(format!(
        "data:{media_type};base64,{encoded}"
    ))))
}

fn resolve_upload_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn infer_media_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "video/mp4",
        Some("mpeg") | Some("mpg") => "video/mpeg",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn validate_media_type(media_type: &str) -> Result<(), VmError> {
    let valid = !media_type.is_empty()
        && media_type
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b';')
        && media_type.split_once('/').is_some();
    if valid {
        Ok(())
    } else {
        Err(VmError::Runtime(format!(
            "mcp.upload_file: invalid media_type {media_type:?}"
        )))
    }
}

pub fn validate_file_inputs_for_call(
    arguments: &JsonValue,
    input_schema: &JsonValue,
) -> Result<(), String> {
    if !schema_contains_file_input(input_schema) {
        return Ok(());
    }
    if !is_enabled() {
        return Err(format!(
            "experimental MCP file upload is disabled; opt in with \
             mcp.configure({{experimental: {{file_upload: {{spec_revision: \"{SPEC_REVISION}\"}}}}}}) \
             before serving {SPEC_URL}"
        ));
    }
    validate_schema_node("$", input_schema, arguments)
}

fn validate_schema_node(path: &str, schema: &JsonValue, value: &JsonValue) -> Result<(), String> {
    if schema.get(X_MCP_FILE).is_some() {
        if !file_schema_is_well_formed(schema) {
            return Err(format!(
                "file input schema at {path} must be type string with format uri"
            ));
        }
        validate_file_value(path, schema, value)?;
    }

    if let (Some(properties), Some(values)) = (
        schema.get("properties").and_then(JsonValue::as_object),
        value.as_object(),
    ) {
        for (key, child_schema) in properties {
            if let Some(child_value) = values.get(key) {
                validate_schema_node(&format!("{path}.{key}"), child_schema, child_value)?;
            }
        }
    }

    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (idx, child_value) in values.iter().enumerate() {
            validate_schema_node(&format!("{path}[{idx}]"), items, child_value)?;
        }
    }

    Ok(())
}

fn validate_file_value(path: &str, schema: &JsonValue, value: &JsonValue) -> Result<(), String> {
    let Some(raw) = value.as_str() else {
        return Err(format!("file input {path} must be a data URI string"));
    };
    let parsed = parse_data_uri(raw).map_err(|error| format!("file input {path}: {error}"))?;
    let descriptor = schema
        .get(X_MCP_FILE)
        .and_then(JsonValue::as_object)
        .expect("checked by caller");

    if let Some(max_size) = descriptor
        .get("maxSize")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("file input schema at {path} has invalid maxSize"))
        })
        .transpose()?
    {
        if parsed.decoded_size > max_size {
            return Err(format!(
                "file input {path} is {} bytes, exceeding maxSize {max_size}",
                parsed.decoded_size
            ));
        }
    }

    let accept = match descriptor.get("accept") {
        Some(JsonValue::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                item.as_str().ok_or_else(|| {
                    format!("file input schema at {path} has non-string accept[{idx}]")
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("file input schema at {path} has invalid accept")),
        None => Vec::new(),
    };
    if !accept.is_empty() && !media_type_matches_accept(&parsed.media_type, &accept) {
        return Err(format!(
            "file input {path} media type {:?} does not match accept {:?}",
            parsed.media_type, accept
        ));
    }

    Ok(())
}

fn file_schema_is_well_formed(schema: &JsonValue) -> bool {
    schema.get("type").and_then(JsonValue::as_str) == Some("string")
        && schema.get("format").and_then(JsonValue::as_str) == Some("uri")
        && schema
            .get(X_MCP_FILE)
            .and_then(JsonValue::as_object)
            .is_some()
}

fn schema_contains_file_input(schema: &JsonValue) -> bool {
    if schema.get(X_MCP_FILE).is_some() {
        return true;
    }
    schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|properties| properties.values().any(schema_contains_file_input))
        || schema.get("items").is_some_and(schema_contains_file_input)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DataUriInfo {
    media_type: String,
    decoded_size: u64,
}

fn parse_data_uri(raw: &str) -> Result<DataUriInfo, &'static str> {
    let rest = raw
        .strip_prefix("data:")
        .ok_or("value must use the data: URI scheme")?;
    let (header, data) = rest
        .split_once(',')
        .ok_or("data URI is missing the comma separator")?;
    let mut parts = header.split(';');
    let media_type = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("text/plain")
        .to_ascii_lowercase();
    if media_type.split_once('/').is_none() {
        return Err("data URI media type must be type/subtype");
    }
    let base64_encoded = parts.any(|part| part.eq_ignore_ascii_case("base64"));
    let decoded_size = if base64_encoded {
        let mut reader = base64::read::DecoderReader::new(
            data.as_bytes(),
            &base64::engine::general_purpose::STANDARD,
        );
        let mut buffer = [0u8; 8192];
        let mut decoded_size = 0u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|_| "base64 payload is malformed")?;
            if read == 0 {
                break decoded_size;
            }
            decoded_size += read as u64;
        }
    } else {
        percent_decoded_len(data)?
    };
    Ok(DataUriInfo {
        media_type,
        decoded_size,
    })
}

fn percent_decoded_len(data: &str) -> Result<u64, &'static str> {
    let bytes = data.as_bytes();
    let mut idx = 0;
    let mut len = 0u64;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len()
                || hex_value(bytes[idx + 1]).is_none()
                || hex_value(bytes[idx + 2]).is_none()
            {
                return Err("percent-encoded payload is malformed");
            }
            idx += 3;
        } else {
            idx += 1;
        }
        len += 1;
    }
    Ok(len)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn media_type_matches_accept(media_type: &str, accept: &[&str]) -> bool {
    let media = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    let Some((media_type_part, media_subtype_part)) = media.split_once('/') else {
        return false;
    };
    let mut saw_mime_pattern = false;
    for raw in accept {
        let pattern = raw.trim().to_ascii_lowercase();
        if pattern.is_empty() || pattern.starts_with('.') {
            continue;
        }
        let Some((type_part, subtype_part)) = pattern.split_once('/') else {
            continue;
        };
        saw_mime_pattern = true;
        if type_part == media_type_part
            && (subtype_part == "*" || subtype_part == media_subtype_part)
        {
            return true;
        }
    }
    !saw_mime_pattern
}

pub fn redact_data_uris_for_logs(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::String(raw) if raw.starts_with("data:") => {
            JsonValue::String(redact_data_uri(raw))
        }
        JsonValue::Array(items) => {
            JsonValue::Array(items.iter().map(redact_data_uris_for_logs).collect())
        }
        JsonValue::Object(map) => JsonValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_data_uris_for_logs(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn redact_data_uri(raw: &str) -> String {
    let media_type = raw
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(',').map(|(header, _)| header))
        .and_then(|header| header.split(';').next())
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    let digest = hex::encode(Sha256::digest(raw.as_bytes()));
    format!("data:{media_type};redacted;sha256={}", &digest[..16])
}

fn string_field(map: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn int_field(map: &BTreeMap<String, VmValue>, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(VmValue::Int(value)) => Some(*value),
        _ => None,
    }
}

fn optional_bool(map: &BTreeMap<String, VmValue>, key: &str) -> Option<bool> {
    match map.get(key) {
        Some(VmValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn list_of_strings(
    value: Option<&VmValue>,
    field: &str,
    callee: &str,
) -> Result<Option<Vec<VmValue>>, VmError> {
    value
        .map(|value| {
            string_vec(value, field, callee).map(|items| {
                items
                    .into_iter()
                    .map(|item| VmValue::String(Rc::from(item)))
                    .collect()
            })
        })
        .transpose()
}

fn string_vec_field(
    map: &BTreeMap<String, VmValue>,
    key: &str,
    callee: &str,
) -> Result<Option<Vec<String>>, VmError> {
    map.get(key)
        .map(|value| string_vec(value, key, callee))
        .transpose()
}

fn string_vec(value: &VmValue, field: &str, callee: &str) -> Result<Vec<String>, VmError> {
    match value {
        VmValue::List(items) => items
            .iter()
            .enumerate()
            .map(|(idx, item)| match item {
                VmValue::String(value) => Ok(value.to_string()),
                _ => Err(VmError::Runtime(format!(
                    "{callee}: {field}[{idx}] must be a string, got {}",
                    item.type_name()
                ))),
            })
            .collect(),
        other => Err(VmError::Runtime(format!(
            "{callee}: {field} must be a list of strings, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_schema() -> JsonValue {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "format": "uri",
                    "x-mcp-file": {
                        "accept": ["image/*"],
                        "maxSize": 4
                    }
                }
            }
        })
    }

    #[test]
    fn validation_is_disabled_until_configured() {
        reset_for_tests();
        let err = validate_file_inputs_for_call(&serde_json::json!({}), &json_schema())
            .expect_err("disabled feature should fail");
        assert!(err.contains("experimental MCP file upload is disabled"));
    }

    #[test]
    fn validates_base64_data_uri_constraints() {
        reset_for_tests();
        FILE_UPLOAD_CONFIG.with(|cell| {
            *cell.borrow_mut() = Some(FileUploadConfig {
                spec_revision: SPEC_REVISION.to_string(),
            });
        });
        validate_file_inputs_for_call(
            &serde_json::json!({"image": "data:image/png;base64,aGk="}),
            &json_schema(),
        )
        .expect("valid data URI");
        let err = validate_file_inputs_for_call(
            &serde_json::json!({"image": "data:text/plain;base64,aGk="}),
            &json_schema(),
        )
        .expect_err("wrong media type should fail");
        assert!(err.contains("does not match accept"));
    }

    #[test]
    fn file_input_schema_rejects_negative_max_size() {
        reset_for_tests();
        FILE_UPLOAD_CONFIG.with(|cell| {
            *cell.borrow_mut() = Some(FileUploadConfig {
                spec_revision: SPEC_REVISION.to_string(),
            });
        });
        let options = VmValue::Dict(Rc::new(BTreeMap::from([(
            "max_size".to_string(),
            VmValue::Int(-1),
        )])));
        let err = file_input_schema(&options).expect_err("negative max_size should fail");
        assert!(err.to_string().contains("max_size must be non-negative"));
    }

    #[test]
    fn redacts_data_uri_without_losing_identity() {
        let redacted = redact_data_uris_for_logs(
            &serde_json::json!({"file": "data:text/plain;base64,aGVsbG8="}),
        );
        let file = redacted["file"].as_str().expect("string");
        assert!(file.starts_with("data:text/plain;redacted;sha256="));
        assert!(!file.contains("aGVsbG8="));
    }

    #[test]
    fn malformed_file_schema_fails_closed() {
        reset_for_tests();
        FILE_UPLOAD_CONFIG.with(|cell| {
            *cell.borrow_mut() = Some(FileUploadConfig {
                spec_revision: SPEC_REVISION.to_string(),
            });
        });
        let err = validate_file_inputs_for_call(
            &serde_json::json!({"image": "data:image/png;base64,aGk="}),
            &serde_json::json!({
                "type": "object",
                "properties": {
                    "image": {
                        "type": "string",
                        "x-mcp-file": {}
                    }
                }
            }),
        )
        .expect_err("malformed schema should fail closed");
        assert!(err.contains("must be type string with format uri"));
    }

    #[test]
    fn malformed_file_descriptor_fails_closed() {
        reset_for_tests();
        FILE_UPLOAD_CONFIG.with(|cell| {
            *cell.borrow_mut() = Some(FileUploadConfig {
                spec_revision: SPEC_REVISION.to_string(),
            });
        });
        let err = validate_file_inputs_for_call(
            &serde_json::json!({"image": "data:image/png;base64,aGk="}),
            &serde_json::json!({
                "type": "object",
                "properties": {
                    "image": {
                        "type": "string",
                        "format": "uri",
                        "x-mcp-file": {"maxSize": -1}
                    }
                }
            }),
        )
        .expect_err("malformed descriptor should fail closed");
        assert!(err.contains("invalid maxSize"));
    }
}
