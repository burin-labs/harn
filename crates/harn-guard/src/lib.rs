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
//! The heavy inference runtime (candle/ONNX) lives behind the off-by-default
//! `neural` feature, so the default binary never links a model runtime. When
//! built without it, [`register_if_available`] is a no-op and the runtime keeps
//! using the always-available built-in heuristic classifier.

pub mod catalog;
mod error;
pub mod resolve;
pub mod store;

pub use catalog::{CatalogFile, CatalogModel, ModelFormat, DEFAULT_MODEL};
pub use error::{GuardError, Result};
pub use resolve::resolve_dir;
pub use store::{sha256_hex, GuardStore, Manifest, ManifestFile};

#[cfg(not(feature = "neural"))]
/// Register the neural classifier into the runtime seam when this binary was
/// built with the `neural` feature and a usable model resolves from `selector`.
///
/// Without the `neural` feature this is a no-op returning `false`: the built-in
/// heuristic classifier (in `harn-vm`) stays active. The `neural` implementation
/// is added in slice 2b.
pub fn register_if_available(_base_dir: &std::path::Path, _selector: &str) -> bool {
    false
}
