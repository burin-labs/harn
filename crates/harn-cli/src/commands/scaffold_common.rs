use std::fs;
use std::path::Path;

use crate::package::PackageError;

pub(crate) fn write_file(
    root: &Path,
    relative_path: &str,
    content: &str,
) -> Result<(), PackageError> {
    write_bytes(root, relative_path, content.as_bytes())
}

pub(crate) fn write_bytes(
    root: &Path,
    relative_path: &str,
    content: &[u8],
) -> Result<(), PackageError> {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    harn_vm::atomic_io::atomic_write(&path, content)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(())
}

pub(crate) fn harn_identifier(name: &str) -> Result<String, PackageError> {
    harn_identifier_with_prefix(name, "harn")
}

pub(crate) fn harn_identifier_with_prefix(
    name: &str,
    leading_digit_prefix: &str,
) -> Result<String, PackageError> {
    let mut out = String::new();
    for ch in name.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        return Err(format!("name {name:?} does not contain a Harn identifier").into());
    }
    if out
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    {
        Ok(out)
    } else {
        Ok(format!("{leading_digit_prefix}_{out}"))
    }
}

pub(crate) fn validate_harn_identifier(value: &str, label: &str) -> Result<(), PackageError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{label} must not be empty").into());
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(format!("{label} {value:?} must start with a letter or underscore").into());
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err(format!(
            "{label} {value:?} must contain only ASCII letters, numbers, or underscores"
        )
        .into());
    }
    Ok(())
}

pub(crate) fn pascal_identifier_from_snake(value: &str) -> String {
    let mut out = String::new();
    for segment in value.split('_').filter(|segment| !segment.is_empty()) {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    if out.is_empty() {
        "Client".to_string()
    } else {
        out
    }
}

pub(crate) fn harn_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
