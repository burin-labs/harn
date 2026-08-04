//! `harn-guard` — the downloadable on-device prompt-injection classifier for
//! Harn (security Layer 2).
//!
//! This crate is the **management layer**: a catalog of upstream, already-hosted
//! models ([`catalog`]), an on-disk store that installs and verifies them
//! ([`store`]), and selector resolution ([`resolve_dir`]). It hosts nothing,
//! bundles no weights, and makes no network calls itself — the CLI downloads
//! from the catalog's upstream URLs on the user's machine and hands the bytes to
//! [`GuardStore::install`] for SHA-256 verification + atomic install.
//!
//! The heavy inference runtime (ONNX) lives behind the off-by-default `neural`
//! feature, so the default binary never links a model runtime. When built
//! without it, [`load_classifier`] is a no-op returning `None` and the runtime
//! keeps using the always-available built-in heuristic classifier. When built
//! with it, a host installs [`load_classifier`] into the runtime's lazy loader
//! seam (`harn_vm::security::set_injection_classifier_loader`), which fires the
//! first time a `local-ml` policy scores untrusted content.

pub mod catalog;
mod error;
#[cfg(any(feature = "neural", test))]
mod neural_math;
pub mod resolve;
pub mod store;

#[cfg(feature = "neural")]
mod neural;

pub use catalog::{CatalogFile, CatalogModel, ModelFormat, DEFAULT_MODEL};
pub use error::{GuardError, Result};
pub use resolve::resolve_dir;
pub use store::{sha256_hex, GuardStore, Manifest, ManifestFile};

#[cfg(feature = "neural")]
pub use neural::OnnxInjectionClassifier;

/// Load the installed model named/located by `selector` into a boxed
/// [`InjectionClassifier`](harn_vm::security::InjectionClassifier), or `None` if
/// the neural backend is unavailable, the model is not installed, or it fails to
/// load. Never panics or registers anything itself — the caller (typically the
/// runtime's lazy loader seam) decides what to do with the result.
///
/// Without the `neural` feature this always returns `None`, so the built-in
/// heuristic classifier stays active and no model runtime is linked.
#[cfg(feature = "neural")]
pub fn load_classifier(
    base_dir: &std::path::Path,
    selector: &str,
) -> Option<Box<dyn harn_vm::security::InjectionClassifier>> {
    let store = GuardStore::new(base_dir);
    let dir = match resolve_dir(&store, selector) {
        Ok(Some(dir)) => dir,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("[harn-guard] cannot resolve guard model `{selector}`: {error}");
            return None;
        }
    };

    // Prefer the installed manifest's recorded id + format; fall back to ONNX
    // for a bring-your-own directory selector.
    let (model_id, format) = match store.read_manifest(selector) {
        Ok(manifest) => (manifest.name, manifest.format),
        Err(_) => (selector.to_owned(), ModelFormat::Onnx),
    };

    match neural::OnnxInjectionClassifier::load(&dir, &model_id, format) {
        Ok(classifier) => Some(Box::new(classifier)),
        Err(error) => {
            eprintln!("[harn-guard] failed to load guard model `{model_id}`: {error}");
            None
        }
    }
}

/// Stub for builds without the `neural` feature: the runtime keeps the heuristic.
#[cfg(not(feature = "neural"))]
pub fn load_classifier(
    _base_dir: &std::path::Path,
    _selector: &str,
) -> Option<Box<dyn harn_vm::security::InjectionClassifier>> {
    None
}
