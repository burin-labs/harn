//! Path and file helpers shared by the orchestration commands that write
//! artifact directories (merge captain, skill gate, eval packs).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::value::VmError;

/// Resolve a manifest-relative `path` against `base_dir`; absolute paths and
/// a missing base pass through unchanged.
pub(super) fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        path_buf
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path_buf)
    } else {
        path_buf
    }
}

/// Reduce `value` to `[A-Za-z0-9_-]` so it is safe as one path segment.
pub(super) fn safe_path_segment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

/// Write `value` as pretty JSON with a trailing newline, creating parents.
pub(super) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), VmError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| VmError::Runtime(format!("failed to serialize JSON artifact: {error}")))?;
    bytes.push(b'\n');
    write_bytes_file(path, &bytes)
}

pub(super) fn write_text_file(path: &Path, value: &str) -> Result<(), VmError> {
    write_bytes_file(path, value.as_bytes())
}

pub(super) fn write_bytes_file(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create artifact directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    fs::write(path, bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write artifact {}: {error}",
            path.display()
        ))
    })
}
