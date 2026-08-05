//! The built-in catalog of downloadable injection-detection models.
//!
//! Catalog entries are *pointers* to already-hosted upstream model repositories
//! (Hugging Face). Harn hosts nothing and bundles no weights — `harn guard
//! install` fetches from these URLs on the user's machine, at the user's
//! request. Integrity-critical weight files carry a pinned SHA-256 (the upstream
//! Git-LFS object id); small config/tokenizer files are recorded
//! trust-on-install.

use serde::{Deserialize, Serialize};

/// On-disk weight format a model ships in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelFormat {
    /// ONNX graph, run by an ONNX runtime backend.
    Onnx,
    /// `safetensors` weights, run by a native-Rust backend (candle).
    Safetensors,
}

impl ModelFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onnx => "onnx",
            Self::Safetensors => "safetensors",
        }
    }
}

/// One file to fetch for a model package.
#[derive(Clone, Copy, Debug)]
pub struct CatalogFile {
    /// Path of the file within the upstream repository (the download source).
    pub remote: &'static str,
    /// Filename to store it under inside the model's install directory.
    pub dest: &'static str,
    /// Pinned SHA-256 (hex) of the file, or `None` to record-on-install (TOFU).
    /// Pinned only where the upstream exposes a content digest (LFS objects).
    pub sha256: Option<&'static str>,
    /// Expected size in bytes, when known. Advisory (drives progress + a cheap
    /// pre-download sanity check); integrity rests on `sha256`.
    pub size: Option<u64>,
}

/// A catalog entry: an upstream, already-hosted model the user may install.
#[derive(Clone, Copy, Debug)]
pub struct CatalogModel {
    /// Stable catalog id used on the CLI and in config (`guard_model`).
    pub name: &'static str,
    /// Human-facing name.
    pub display_name: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Upstream repository id (e.g. a Hugging Face `org/repo`).
    pub repo: &'static str,
    /// Base URL files are resolved against (`{base}/{remote}`).
    pub base_url: &'static str,
    /// SPDX-ish license identifier.
    pub license_id: &'static str,
    /// URL where the license / acceptable-use terms can be read.
    pub license_url: &'static str,
    /// `true` when the upstream requires authentication + license acceptance
    /// (e.g. a gated Hugging Face repo). Install then requires `HF_TOKEN`.
    pub gated: bool,
    /// Weight format.
    pub format: ModelFormat,
    /// Files to fetch. The first whose `dest` ends in the format extension is the
    /// weight file; the rest are tokenizer/config.
    pub files: &'static [CatalogFile],
}

impl CatalogModel {
    /// Resolve the absolute download URL for `file`.
    pub fn url_for(&self, file: &CatalogFile) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), file.remote)
    }

    /// Total advertised download size, when every file advertises one.
    pub fn total_size(&self) -> Option<u64> {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The default model: ungated, permissively licensed, zero-friction.
pub const DEFAULT_MODEL: &str = "deberta-v3-prompt-injection-v2";

// ProtectAI DeBERTa-v3 prompt-injection classifier — Apache-2.0, ungated. The
// ONNX weight + tokenizer + config are everything the inference backend needs.
const DEBERTA_FILES: &[CatalogFile] = &[
    CatalogFile {
        remote: "onnx/model.onnx",
        dest: "model.onnx",
        sha256: Some("f0ea7f239f765aedbde7c9e163a7cb38a79c5b8853d3f76db5152172047b228c"),
        size: Some(738_563_188),
    },
    CatalogFile {
        remote: "onnx/tokenizer.json",
        dest: "tokenizer.json",
        sha256: None,
        size: Some(8_648_886),
    },
    CatalogFile {
        remote: "onnx/config.json",
        dest: "config.json",
        sha256: None,
        size: Some(1_014),
    },
];

// Meta Llama Prompt Guard 2 (community ONNX export) — gated (Llama Community
// License + Hugging Face access request). Listed as an opt-in; install requires
// the user's own HF_TOKEN and explicit license acceptance. We pin nothing we
// cannot fetch anonymously, so the weight digests are recorded on install.
const PROMPT_GUARD_2_86M_FILES: &[CatalogFile] = &[
    CatalogFile {
        remote: "model.onnx",
        dest: "model.onnx",
        sha256: None,
        size: None,
    },
    CatalogFile {
        remote: "tokenizer.json",
        dest: "tokenizer.json",
        sha256: None,
        size: None,
    },
    CatalogFile {
        remote: "config.json",
        dest: "config.json",
        sha256: None,
        size: None,
    },
];

const CATALOG: &[CatalogModel] = &[
    CatalogModel {
        name: DEFAULT_MODEL,
        display_name: "ProtectAI DeBERTa-v3 prompt-injection v2",
        description: "Apache-2.0, ungated. Recommended default — no account or license gate.",
        repo: "protectai/deberta-v3-base-prompt-injection-v2",
        base_url:
            "https://huggingface.co/protectai/deberta-v3-base-prompt-injection-v2/resolve/main",
        license_id: "Apache-2.0",
        license_url: "https://huggingface.co/protectai/deberta-v3-base-prompt-injection-v2",
        gated: false,
        format: ModelFormat::Onnx,
        files: DEBERTA_FILES,
    },
    CatalogModel {
        name: "llama-prompt-guard-2-86m",
        display_name: "Meta Llama Prompt Guard 2 (86M, ONNX)",
        description: "Higher recall, but GATED — needs Hugging Face access + HF_TOKEN.",
        repo: "gravitee-io/Llama-Prompt-Guard-2-86M-onnx",
        base_url: "https://huggingface.co/gravitee-io/Llama-Prompt-Guard-2-86M-onnx/resolve/main",
        license_id: "Llama-Community",
        license_url: "https://huggingface.co/meta-llama/Llama-Prompt-Guard-2-86M",
        gated: true,
        format: ModelFormat::Onnx,
        files: PROMPT_GUARD_2_86M_FILES,
    },
];

/// All catalog models.
pub fn all() -> &'static [CatalogModel] {
    CATALOG
}

/// Find a catalog model by its stable `name`.
pub fn find(name: &str) -> Option<&'static CatalogModel> {
    CATALOG.iter().find(|m| m.name == name)
}

/// The recommended default model entry.
pub fn default_model() -> &'static CatalogModel {
    find(DEFAULT_MODEL).expect("default model is always present in the catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_ungated_and_present() {
        let model = default_model();
        assert_eq!(model.name, DEFAULT_MODEL);
        assert!(!model.gated, "the recommended default must be ungated");
        assert_eq!(model.license_id, "Apache-2.0");
    }

    #[test]
    fn default_model_matches_runtime_default_selector() {
        // `harn-vm` carries the default guard-model selector independently (to
        // stay free of a dependency on `harn-guard`); the two must not drift.
        assert_eq!(DEFAULT_MODEL, harn_vm::config::DEFAULT_GUARD_MODEL);
    }

    #[test]
    fn catalog_entries_are_well_formed() {
        for model in all() {
            assert!(!model.name.is_empty());
            assert!(!model.files.is_empty(), "{} has no files", model.name);
            assert!(
                model.base_url.starts_with("https://"),
                "{} base_url must be https",
                model.name
            );
            // Any pinned digest must be a 64-char lowercase hex string.
            for file in model.files {
                if let Some(sha) = file.sha256 {
                    assert_eq!(sha.len(), 64, "{}/{} sha len", model.name, file.dest);
                    assert!(
                        sha.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                        "{}/{} sha must be lowercase hex",
                        model.name,
                        file.dest
                    );
                }
            }
        }
    }

    #[test]
    fn url_for_joins_base_and_remote() {
        let model = default_model();
        let file = &model.files[0];
        assert_eq!(
            model.url_for(file),
            "https://huggingface.co/protectai/deberta-v3-base-prompt-injection-v2/resolve/main/onnx/model.onnx"
        );
    }

    #[test]
    fn gated_model_is_opt_in_only() {
        let gated = find("llama-prompt-guard-2-86m").expect("present");
        assert!(gated.gated);
        // We never pin digests we cannot fetch anonymously.
        assert!(gated.files.iter().all(|f| f.sha256.is_none()));
    }
}
