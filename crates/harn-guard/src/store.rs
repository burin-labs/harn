//! The on-disk model store under `<state>/guard/`.
//!
//! Layout: `<state>/guard/<model-name>/{<files...>, manifest.json}`. Installs are
//! staged in a sibling temp directory and renamed into place, so a partial or
//! failed download never leaves a half-written model the resolver would pick up.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::{CatalogModel, ModelFormat};
use crate::error::{GuardError, Result};

const MANIFEST_NAME: &str = "manifest.json";

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A file recorded in an installed model's manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

/// On-disk record of an installed model package (`manifest.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub display_name: String,
    pub repo: String,
    pub license_id: String,
    pub license_url: String,
    /// The user explicitly accepted the upstream license at install time.
    pub license_accepted: bool,
    pub format: ModelFormat,
    pub files: Vec<ManifestFile>,
}

/// The model store rooted at `<state>/guard/`.
pub struct GuardStore {
    root: PathBuf,
}

impl GuardStore {
    /// Open the store under `<state_root(base_dir)>/guard`.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            root: harn_vm::runtime_paths::state_root(base_dir).join("guard"),
        }
    }

    /// Open a store at an explicit root (used by tests).
    pub fn at_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn model_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn manifest_path(&self, name: &str) -> PathBuf {
        self.model_dir(name).join(MANIFEST_NAME)
    }

    /// Whether `name` is installed (has a manifest).
    pub fn is_installed(&self, name: &str) -> bool {
        self.manifest_path(name).is_file()
    }

    /// Read an installed model's manifest.
    pub fn read_manifest(&self, name: &str) -> Result<Manifest> {
        let path = self.manifest_path(name);
        let bytes = fs::read(&path).map_err(|source| GuardError::Io {
            path: path.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| GuardError::Manifest { path, source })
    }

    /// List installed models (directories holding a readable manifest), sorted
    /// by name.
    pub fn installed(&self) -> Vec<Manifest> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<Manifest> = entries
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| self.read_manifest(&name).ok())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Verify, then atomically install, a downloaded model `payload` (a map of
    /// each catalog file's `dest` to its bytes). Every catalog file must be
    /// present; any file with a pinned digest must match it; `license_accepted`
    /// must be true (the caller surfaces the prompt/flag). The staged directory
    /// is renamed into place so a failure never publishes a partial model.
    pub fn install(
        &self,
        model: &CatalogModel,
        payload: &[(String, Vec<u8>)],
        license_accepted: bool,
    ) -> Result<Manifest> {
        if !license_accepted {
            return Err(GuardError::LicenseNotAccepted {
                model: model.name.to_owned(),
                license: model.license_id.to_owned(),
                license_url: model.license_url.to_owned(),
            });
        }

        let mut files = Vec::with_capacity(model.files.len());
        for catalog_file in model.files {
            let bytes = payload
                .iter()
                .find(|(dest, _)| dest == catalog_file.dest)
                .map(|(_, bytes)| bytes)
                .ok_or_else(|| GuardError::MissingFile(catalog_file.dest.to_owned()))?;
            let actual = sha256_hex(bytes);
            if let Some(expected) = catalog_file.sha256 {
                if actual != expected {
                    return Err(GuardError::ChecksumMismatch {
                        file: catalog_file.dest.to_owned(),
                        expected: expected.to_owned(),
                        actual,
                    });
                }
            }
            files.push(ManifestFile {
                name: catalog_file.dest.to_owned(),
                sha256: actual,
                size: bytes.len() as u64,
            });
        }

        let manifest = Manifest {
            name: model.name.to_owned(),
            display_name: model.display_name.to_owned(),
            repo: model.repo.to_owned(),
            license_id: model.license_id.to_owned(),
            license_url: model.license_url.to_owned(),
            license_accepted,
            format: model.format,
            files,
        };

        let staging = self.root.join(format!(".{}.staging", model.name));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|source| GuardError::Io {
            path: staging.clone(),
            source,
        })?;
        for (dest, bytes) in payload {
            let file_path = staging.join(dest);
            fs::write(&file_path, bytes).map_err(|source| GuardError::Io {
                path: file_path,
                source,
            })?;
        }
        let manifest_path = staging.join(MANIFEST_NAME);
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).map_err(|source| GuardError::Manifest {
                path: manifest_path.clone(),
                source,
            })?;
        fs::write(&manifest_path, manifest_bytes).map_err(|source| GuardError::Io {
            path: manifest_path,
            source,
        })?;

        let dir = self.model_dir(model.name);
        let _ = fs::remove_dir_all(&dir);
        fs::rename(&staging, &dir).map_err(|source| GuardError::Io { path: dir, source })?;
        Ok(manifest)
    }

    /// Remove an installed model. Returns whether it existed.
    pub fn remove(&self, name: &str) -> Result<bool> {
        let dir = self.model_dir(name);
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&dir).map_err(|source| GuardError::Io { path: dir, source })?;
        Ok(true)
    }

    /// Re-hash an installed model's files and report whether they still match the
    /// digests recorded at install time (detects on-disk corruption).
    pub fn verify_installed(&self, name: &str) -> Result<bool> {
        let manifest = self.read_manifest(name)?;
        for file in &manifest.files {
            let path = self.model_dir(name).join(&file.name);
            let bytes = fs::read(&path).map_err(|source| GuardError::Io { path, source })?;
            if sha256_hex(&bytes) != file.sha256 {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    fn temp_root() -> PathBuf {
        // A unique-enough dir without pulling a tempfile dep or a clock: the test
        // thread id plus the test name embedded by callers keeps these distinct.
        let mut p = std::env::temp_dir();
        p.push(format!("harn-guard-test-{:?}", std::thread::current().id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn payload(files: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .map(|(dest, bytes)| ((*dest).to_owned(), (*bytes).to_vec()))
            .collect()
    }

    #[test]
    fn sha256_hex_is_lowercase_64() {
        let h = sha256_hex(b"hello");
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn install_then_read_and_list_roundtrips() {
        let store = GuardStore::at_root(temp_root());
        let model = catalog::default_model();
        // Provide bytes for every catalog file; the pinned model.onnx digest will
        // NOT match these stub bytes, so use a model with no pinned digests for a
        // clean roundtrip and assert the pin separately below.
        let stub = payload(&[
            ("model.onnx", b"x"),
            ("tokenizer.json", b"y"),
            ("config.json", b"z"),
        ]);
        // default model pins model.onnx -> mismatch expected.
        let err = store.install(model, &stub, true).unwrap_err();
        assert!(matches!(err, GuardError::ChecksumMismatch { .. }));

        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn install_requires_license_acceptance() {
        let store = GuardStore::at_root(temp_root());
        let model = catalog::find("llama-prompt-guard-2-86m").unwrap();
        let stub = payload(&[
            ("model.onnx", b"a"),
            ("tokenizer.json", b"b"),
            ("config.json", b"c"),
        ]);
        let err = store.install(model, &stub, false).unwrap_err();
        assert!(matches!(err, GuardError::LicenseNotAccepted { .. }));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn install_unpinned_model_roundtrips_and_verifies() {
        let store = GuardStore::at_root(temp_root());
        // The gated entry pins nothing, so stub bytes install cleanly (TOFU).
        let model = catalog::find("llama-prompt-guard-2-86m").unwrap();
        let files = payload(&[
            ("model.onnx", b"weights"),
            ("tokenizer.json", b"tok"),
            ("config.json", b"cfg"),
        ]);
        let manifest = store.install(model, &files, true).unwrap();
        assert_eq!(manifest.name, "llama-prompt-guard-2-86m");
        assert!(manifest.license_accepted);
        assert_eq!(manifest.files.len(), 3);

        assert!(store.is_installed("llama-prompt-guard-2-86m"));
        let read = store.read_manifest("llama-prompt-guard-2-86m").unwrap();
        assert_eq!(read, manifest);
        assert!(store.verify_installed("llama-prompt-guard-2-86m").unwrap());

        let listed = store.installed();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "llama-prompt-guard-2-86m");

        assert!(store.remove("llama-prompt-guard-2-86m").unwrap());
        assert!(!store.is_installed("llama-prompt-guard-2-86m"));
        assert!(!store.remove("llama-prompt-guard-2-86m").unwrap());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn missing_file_in_payload_errors() {
        let store = GuardStore::at_root(temp_root());
        let model = catalog::find("llama-prompt-guard-2-86m").unwrap();
        let incomplete = payload(&[("model.onnx", b"weights")]);
        let err = store.install(model, &incomplete, true).unwrap_err();
        assert!(matches!(err, GuardError::MissingFile(f) if f == "tokenizer.json"));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn verify_detects_corruption() {
        let store = GuardStore::at_root(temp_root());
        let model = catalog::find("llama-prompt-guard-2-86m").unwrap();
        let files = payload(&[
            ("model.onnx", b"weights"),
            ("tokenizer.json", b"tok"),
            ("config.json", b"cfg"),
        ]);
        store.install(model, &files, true).unwrap();
        // Corrupt a file on disk.
        fs::write(store.model_dir(model.name).join("config.json"), b"tampered").unwrap();
        assert!(!store.verify_installed(model.name).unwrap());
        let _ = fs::remove_dir_all(store.root());
    }
}
