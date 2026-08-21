//! Resolve a model selector to a concrete directory on disk.

use std::path::{Path, PathBuf};

use crate::catalog::ModelPurpose;
use crate::error::{GuardError, Result};
use crate::store::GuardStore;

/// Resolve `selector` to a model directory.
///
/// - A selector containing a path separator (or an absolute path) is treated as
///   a bring-your-own model directory and must exist.
/// - Otherwise it is a catalog name; the store is consulted. A catalog name that
///   is simply not installed yields `Ok(None)` — detection then falls back to
///   the heuristic rather than failing the run.
pub fn resolve_dir(
    store: &GuardStore,
    selector: &str,
    purpose: ModelPurpose,
) -> Result<Option<PathBuf>> {
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
    if store.is_installed(selector, purpose) {
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
        let tmp = tempfile::tempdir().unwrap();
        let store = GuardStore::at_root(tmp.path().to_path_buf());
        assert!(
            resolve_dir(&store, "", ModelPurpose::InjectionClassification)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uninstalled_catalog_name_is_none_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GuardStore::at_root(tmp.path().to_path_buf());
        assert!(resolve_dir(
            &store,
            "deberta-v3-prompt-injection-v2",
            ModelPurpose::InjectionClassification
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn nonexistent_path_selector_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GuardStore::at_root(tmp.path().to_path_buf());
        let missing = store.root().join("no-such").join("guard-model-dir");
        let selector = missing.to_string_lossy();
        let err =
            resolve_dir(&store, &selector, ModelPurpose::InjectionClassification).unwrap_err();
        assert!(matches!(err, GuardError::PathNotFound(_)));
    }

    #[test]
    fn installed_catalog_model_resolves_only_for_its_purpose() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GuardStore::at_root(tmp.path().join("guard"));
        let model = crate::catalog::find(
            ModelPurpose::InjectionClassification,
            "llama-prompt-guard-2-86m",
        )
        .unwrap();
        let payload = [
            ("model.onnx".to_owned(), b"weights".to_vec()),
            ("tokenizer.json".to_owned(), b"tokenizer".to_vec()),
            ("config.json".to_owned(), b"config".to_vec()),
        ];
        store.install(model, &payload, true).unwrap();

        assert_eq!(
            resolve_dir(&store, model.name, ModelPurpose::InjectionClassification).unwrap(),
            Some(store.model_dir(model.name))
        );
        assert!(resolve_dir(&store, model.name, ModelPurpose::Embedding)
            .unwrap()
            .is_none());
    }
}
