use std::collections::BTreeMap;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

use super::{
    io_error_value, io_error_value_with_kind, queue_file_edited_for, resolve_fs_path, result_err,
    result_ok, FILE_TEXT_CACHE,
};

fn option_error(message: impl AsRef<str>) -> VmValue {
    io_error_value_with_kind("invalid_input", message)
}

fn parse_options(
    args: &[VmValue],
) -> Result<crate::conditional_replace::ConditionalReplaceOptions, VmValue> {
    let mut options = crate::conditional_replace::ConditionalReplaceOptions {
        durability: crate::atomic_io::AtomicWriteDurability::Namespace,
        ..crate::conditional_replace::ConditionalReplaceOptions::default()
    };
    let Some(value) = args.get(2) else {
        return Ok(options);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(options);
    }
    let VmValue::Dict(values) = value else {
        return Err(option_error("replace options must be a dict"));
    };
    for key in values.keys() {
        if !matches!(
            key.as_str(),
            "expected_sha256" | "create" | "overwrite" | "create_parents" | "durability"
        ) {
            return Err(option_error(format!("unknown replacement option '{key}'")));
        }
    }
    if let Some(value) = values.get("expected_sha256") {
        let VmValue::String(digest) = value else {
            return Err(option_error("expected_sha256 must be a string"));
        };
        let hex = digest.strip_prefix("sha256:").unwrap_or_default();
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(option_error(
                "expected_sha256 must use lowercase sha256:<64 hex digits>",
            ));
        }
        options.expected_sha256 = Some(digest.to_string());
    }
    for (key, target) in [
        ("create", &mut options.create),
        ("overwrite", &mut options.overwrite),
        ("create_parents", &mut options.create_parents),
    ] {
        if let Some(value) = values.get(key) {
            let VmValue::Bool(value) = value else {
                return Err(option_error(format!("{key} must be a bool")));
            };
            *target = *value;
        }
    }
    if let Some(value) = values.get("durability") {
        let VmValue::String(value) = value else {
            return Err(option_error("durability must be a string"));
        };
        options.durability = match value.as_str() {
            "namespace" => crate::atomic_io::AtomicWriteDurability::Namespace,
            "flush" => crate::atomic_io::AtomicWriteDurability::Flush,
            _ => {
                return Err(option_error("durability must be 'namespace' or 'flush'"));
            }
        };
    }
    Ok(options)
}

fn receipt_value(
    receipt: crate::conditional_replace::ConditionalReplaceReceipt,
    durability: crate::atomic_io::AtomicWriteDurability,
) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_str("status", receipt.status.as_str());
    value.put_bool("before_exists", receipt.before_exists);
    value.put_str("before_sha256", receipt.before_sha256);
    value.put_str("after_sha256", receipt.after_sha256);
    value.put_opt_str("expected_sha256", receipt.expected_sha256);
    value.put_int(
        "bytes_written",
        i64::try_from(receipt.bytes_written).unwrap_or(i64::MAX),
    );
    value.put_str(
        "durability",
        match durability {
            crate::atomic_io::AtomicWriteDurability::Namespace => "namespace",
            crate::atomic_io::AtomicWriteDurability::Flush => "flush",
        },
    );
    value.put_bool("file_synced", receipt.file_synced);
    value.put_bool("namespace_synced", receipt.namespace_synced);
    VmValue::dict(value)
}

fn replace_value(builtin: &str, args: &[VmValue], bytes: &[u8]) -> Result<VmValue, VmValue> {
    let path = args.first().map(VmValue::display).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        builtin,
        &resolved,
        crate::stdlib::sandbox::FsAccess::Write,
    )
    .map_err(|error| match error {
        VmError::Thrown(value) => value,
        VmError::CategorizedError { message, category } => {
            let mut failure = BTreeMap::new();
            failure.put_str("error", "io_error");
            failure.put_str("kind", "sandbox_denied");
            failure.put_str("message", message);
            failure.put_str("category", category.as_str());
            VmValue::dict(failure)
        }
        other => option_error(other.to_string()),
    })?;
    let options = parse_options(args)?;
    let receipt =
        crate::testbench::overlay_fs::helpers::replace_scoped(builtin, &resolved, bytes, &options)
            .map_err(|error| {
                io_error_value(
                    format!("Failed to replace file {}: {error}", resolved.display()),
                    &error,
                )
            })?;
    if matches!(
        receipt.status,
        crate::conditional_replace::ConditionalReplaceStatus::Created
            | crate::conditional_replace::ConditionalReplaceStatus::Replaced
    ) {
        FILE_TEXT_CACHE.with(|cache| {
            cache.borrow_mut().remove(&resolved);
        });
        queue_file_edited_for(&resolved, "replace", receipt.bytes_written);
    }
    Ok(receipt_value(receipt, options.durability))
}

fn replace_text_value(builtin: &str, args: &[VmValue]) -> Result<VmValue, VmValue> {
    let content = args.get(1).map(VmValue::display).unwrap_or_default();
    replace_value(builtin, args, content.as_bytes())
}

fn replace_bytes_value(builtin: &str, args: &[VmValue]) -> Result<VmValue, VmValue> {
    let Some(VmValue::Bytes(content)) = args.get(1) else {
        return Err(option_error("replacement content must be bytes"));
    };
    replace_value(builtin, args, content)
}

#[harn_builtin(
    sig = "replace_file(path: string, content: string, options?: dict) -> dict",
    category = "fs",
    doc = "Atomically replace text when an optional observed digest still matches."
)]
fn replace_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    replace_text_value("replace_file", args).map_err(VmError::Thrown)
}

#[harn_builtin(
    sig = "replace_file_result(path: string, content: string, options?: dict) -> Result<dict, dict>",
    category = "fs",
    doc = "Result form of replace_file."
)]
fn replace_file_result_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(match replace_text_value("replace_file_result", args) {
        Ok(receipt) => result_ok(receipt),
        Err(failure) => result_err(failure),
    })
}

#[harn_builtin(
    sig = "replace_file_bytes(path: string, content: bytes, options?: dict) -> dict",
    category = "fs",
    doc = "Atomically replace bytes when an optional observed digest still matches."
)]
fn replace_file_bytes_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    replace_bytes_value("replace_file_bytes", args).map_err(VmError::Thrown)
}

#[harn_builtin(
    sig = "replace_file_bytes_result(path: string, content: bytes, options?: dict) -> Result<dict, dict>",
    category = "fs",
    doc = "Result form of replace_file_bytes."
)]
fn replace_file_bytes_result_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(
        match replace_bytes_value("replace_file_bytes_result", args) {
            Ok(receipt) => result_ok(receipt),
            Err(failure) => result_err(failure),
        },
    )
}
