//! Re-export of `harn_modules::asset_paths` plus a VM-side convenience
//! that anchors the project-root walk at the *currently-executing*
//! source file (via `VM_SOURCE_DIR`) and falls back to the existing
//! source-relative resolver when the path is not an `@`-prefixed asset
//! reference.
//!
//! See `harn_modules::asset_paths` for the resolver itself (issue #742).

use std::path::{Path, PathBuf};

pub use harn_modules::asset_paths::{is_asset_path, parse, resolve, AssetRef};

/// Resolve an `@`-prefixed asset path against the project root, or fall
/// back to the legacy source-relative resolver when `path` is plain.
///
/// `caller_file` is optional. When `None`, the resolver anchors at the
/// VM's thread-local source dir — the file currently executing, which
/// `set_thread_source_dir` keeps in lockstep with imported modules
/// (see `docs/src/modules.md` "source-relative builtins" note).
pub fn resolve_or_source_relative(
    path: &str,
    caller_file: Option<&Path>,
) -> Result<PathBuf, String> {
    let anchor = caller_file
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(super::process::source_root_path);
    harn_modules::asset_paths::resolve_or(path, &anchor, |p| {
        super::process::resolve_source_asset_path(p)
    })
}
