//! Helpers shared by the AST-precise edit builtins (`apply_node`,
//! `insert_at_anchor`, …). Each is small on its own, but every edit
//! primitive needs them and copy-pasting them per file makes the wire
//! contract (post-edit validation, staged-fs routing, response shape)
//! easy to drift out of sync.

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Query, QueryError, QueryErrorKind};

use crate::error::HostlibError;

use super::language::Language;
use super::parse::parse_source;

/// Resolve which capture in `query` drives the edit.
///
/// - Explicit name match wins.
/// - Single-capture queries fall back to the only capture.
/// - Otherwise returns a diagnostic listing the available captures so
///   the caller can fix the query.
pub(super) fn resolve_target_capture(query: &Query, requested: &str) -> Result<u32, String> {
    let names = query.capture_names();
    if let Some(idx) = names.iter().position(|n| *n == requested) {
        return Ok(idx as u32);
    }
    if names.len() == 1 {
        return Ok(0);
    }
    Err(format!(
        "query has no capture named `{requested}`; available captures: [{}]",
        names
            .iter()
            .map(|n| format!("@{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Render a tree-sitter [`QueryErrorKind`] as a short snake-case label
/// suitable for wire responses.
pub(super) fn query_error_kind_str(kind: &QueryErrorKind) -> &'static str {
    match kind {
        QueryErrorKind::Syntax => "syntax",
        QueryErrorKind::NodeType => "node_type",
        QueryErrorKind::Field => "field",
        QueryErrorKind::Capture => "capture",
        QueryErrorKind::Predicate => "predicate",
        QueryErrorKind::Structure => "structure",
        QueryErrorKind::Language => "language",
    }
}

/// Format a tree-sitter [`QueryError`] as the `details` string used in
/// `invalid_query` responses.
pub(super) fn format_query_error(err: &QueryError) -> String {
    format!(
        "tree-sitter rejected query at row {} col {}: {} ({})",
        err.row,
        err.column,
        err.message,
        query_error_kind_str(&err.kind),
    )
}

/// Re-parse `source` for `language` and, if the post-edit tree carries
/// any ERROR / MISSING node, return a short human-readable diagnostic
/// pinpointing the first offender. Returns `None` when the source
/// parses cleanly.
pub(super) fn first_syntax_error(source: &str, language: Language) -> Option<String> {
    let tree = parse_source(source, language).ok()?;
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let pos = node.start_position();
            return Some(format!(
                "missing `{}` at line {}, column {}",
                node.kind(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        if node.is_error() {
            let pos = node.start_position();
            let snippet = node_text(node, source);
            let trimmed: String = snippet.chars().take(40).collect();
            return Some(format!(
                "unexpected `{trimmed}` at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ));
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i as u32) {
                if child.has_error() || child.is_missing() {
                    stack.push(child);
                }
            }
        }
    }
    Some("post-edit source has parse errors".into())
}

pub(super) fn node_text(node: Node<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    if start >= end {
        return String::new();
    }
    std::str::from_utf8(&bytes[start..end])
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Hex sha256 of the given bytes; consumed by every edit response for
/// before/after fingerprinting.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Read `path` through staged-fs when `session_id` is set, otherwise
/// fall back to the real filesystem. Truncates to `max_bytes` (`0` is
/// unlimited). `builtin` names the caller for error reporting.
pub(super) fn read_source(
    builtin: &'static str,
    path: &Path,
    session_id: Option<&str>,
    max_bytes: usize,
) -> Result<String, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, session_id) {
        result.map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    } else {
        std::fs::read(path).map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    };
    let slice = if max_bytes == 0 || bytes.len() <= max_bytes {
        &bytes[..]
    } else {
        &bytes[..max_bytes]
    };
    Ok(String::from_utf8_lossy(slice).into_owned())
}

/// Write `contents` to `path`. Routes through staged-fs when active;
/// otherwise captures an auto-snapshot (#1722 fallback) and writes
/// directly. Creates the parent directory if it doesn't exist.
pub(super) fn write_source(
    builtin: &'static str,
    path: &Path,
    contents: &str,
    session_id: Option<&str>,
) -> Result<(), HostlibError> {
    if crate::fs::stage_write_or_none(builtin, path, contents.as_bytes(), true, true, session_id)?
        .is_some()
    {
        return Ok(());
    }
    crate::fs_snapshot::auto_capture_for_write(builtin, path);
    let owned = PathBuf::from(path);
    if let Some(parent) = owned.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                builtin,
                message: format!("mkdir `{}`: {err}", parent.display()),
            })?;
        }
    }
    std::fs::write(path, contents).map_err(|err| HostlibError::Backend {
        builtin,
        message: format!("write `{}`: {err}", path.display()),
    })
}
