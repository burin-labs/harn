//! Error type for the guard package manager.

use std::path::PathBuf;

/// Failures from installing, resolving, or removing a guard model package.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    /// The requested catalog model name is not in the built-in catalog.
    #[error("unknown guard model `{0}`; run `harn guard list` to see available models")]
    UnknownModel(String),

    /// A gated model was requested without an accepted license / credentials.
    #[error(
        "`{model}` is gated under {license}: accept the license at {license_url} and set HF_TOKEN, \
         then re-run with --accept-license"
    )]
    Gated {
        model: String,
        license: String,
        license_url: String,
    },

    /// Installation was attempted without accepting the model's license.
    #[error(
        "installing `{model}` requires accepting its license ({license} — {license_url}); \
         pass --accept-license to confirm"
    )]
    LicenseNotAccepted {
        model: String,
        license: String,
        license_url: String,
    },

    /// A downloaded file's SHA-256 did not match the catalog's pinned digest.
    #[error("integrity check failed for `{file}`: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    /// A required file was missing from the supplied install payload.
    #[error("install payload is missing required file `{0}`")]
    MissingFile(String),

    /// The selector resolved to a filesystem path that does not exist.
    #[error("guard model path does not exist: {0}")]
    PathNotFound(PathBuf),

    /// An on-disk manifest could not be parsed.
    #[error("malformed guard manifest at {path}: {source}")]
    Manifest {
        path: PathBuf,
        source: serde_json::Error,
    },

    /// Filesystem I/O error.
    #[error("guard store I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Result alias for guard operations.
pub type Result<T> = std::result::Result<T, GuardError>;
