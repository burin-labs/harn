//! Replay-trace fixture discovery shared by `harn bench replay`,
//! `harn orchestrator replay-oracle`, and protocol conformance.

use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a user-supplied fixture selection: absolute or existing paths are
/// taken as-is, other relative paths are repo-relative, and no selection
/// means `default_relative` under the repo root.
pub(crate) fn resolve_selection(
    selection: Option<&Path>,
    repo_root: &Path,
    default_relative: &str,
) -> PathBuf {
    match selection {
        Some(selection) if selection.is_absolute() || selection.exists() => selection.to_path_buf(),
        Some(selection) => repo_root.join(selection),
        None => repo_root.join(default_relative),
    }
}

/// Collect `.json` files at `path` (a file or a directory tree) in sorted
/// order. Unreadable directories contribute nothing.
pub(crate) fn collect_json_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        collect_json_files(&entry.path(), out);
    }
}

pub(crate) fn read_trace_fixture(path: &Path) -> Result<harn_vm::ReplayOracleTrace, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("invalid replay trace JSON in {}: {error}", path.display()))
}

/// Every protocol fixture a trace cites must be a repo-relative file under
/// `conformance/protocols/fixtures/`.
pub(crate) fn validate_protocol_fixture_refs(
    repo_root: &Path,
    refs: &[String],
) -> Result<(), String> {
    for fixture_ref in refs {
        if !fixture_ref.starts_with("conformance/protocols/fixtures/") {
            return Err(format!(
                "protocol fixture ref must point under conformance/protocols/fixtures: {fixture_ref}"
            ));
        }
        let path = Path::new(fixture_ref);
        if path.is_absolute() {
            return Err(format!(
                "protocol fixture ref must be repo-relative: {fixture_ref}"
            ));
        }
        let candidate = repo_root.join(path);
        if !candidate.is_file() {
            return Err(format!(
                "protocol fixture ref not found: {}",
                candidate.display()
            ));
        }
    }
    Ok(())
}

/// The nearest ancestor of the working directory that looks like the Harn
/// repository root.
pub(crate) fn discover_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .find(|ancestor| {
            ancestor.join("Cargo.toml").is_file() && ancestor.join("conformance").is_dir()
        })
        .map(Path::to_path_buf)
}

/// `path` relative to the repo root when possible, always `/`-separated.
pub(crate) fn display_path(path: &Path) -> String {
    discover_repo_root()
        .and_then(|root| path.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
