//! Resolve a model selector to a concrete directory on disk.

use std::path::{Path, PathBuf};

use crate::error::{GuardError, Result};
use crate::store::GuardStore;

/// Resolve `selector` to a model directory.
///
/// - A selector containing a path separator (or an absolute path) is treated as
///   a bring-your-own model directory and must exist.
/// - Otherwise it is a catalog name; the store is consulted. A catalog name that
///   is simply not installed yields `Ok(None)` — detection then falls back to
///   the heuristic rather than failing the run.
pub fn resolve_dir(store: &GuardStore, selector: &str) -> Result<Option<PathBuf>> {
    if selector.is_empty() {
        return Ok(None);
    }
    let path = Path::new(selector);
    if path.is_absolute() || selector.contains(std::path::MAIN_SEPARATOR) {
        return if path.is_dir() {
            Ok(Some(path.to_path_buf()))
        } else {
            Err(GuardError::PathNotFound(path.to_path_buf()))
        };
    }
    if store.is_installed(selector) {
        Ok(Some(store.model_dir(selector)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_selector_resolves_to_none() {
        let store = GuardStore::at_root(std::env::temp_dir().join("harn-guard-resolve-empty"));
        assert!(resolve_dir(&store, "").unwrap().is_none());
    }

    #[test]
    fn uninstalled_catalog_name_is_none_not_error() {
        let store = GuardStore::at_root(std::env::temp_dir().join("harn-guard-resolve-missing"));
        assert!(resolve_dir(&store, "deberta-v3-prompt-injection-v2")
            .unwrap()
            .is_none());
    }

    #[test]
    fn nonexistent_path_selector_errors() {
        let store = GuardStore::at_root(std::env::temp_dir().join("harn-guard-resolve-path"));
        let err = resolve_dir(&store, "/no/such/guard/model/dir").unwrap_err();
        assert!(matches!(err, GuardError::PathNotFound(_)));
    }
}
