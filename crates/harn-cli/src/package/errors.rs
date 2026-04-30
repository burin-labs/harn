//! Typed error enum for the `package` module.
//!
//! Variants carry the exact user-facing message via `Display`, keeping CLI
//! output byte-for-byte identical to the previous `Result<T, String>` API
//! while letting library callers pattern-match on the error category.

use std::result::Result as StdResult;

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("{0}")]
    Manifest(String),
    #[error("{0}")]
    Lockfile(String),
    #[error("{0}")]
    Registry(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Ops(String),
    #[error("{0}")]
    Extensions(String),
    #[error("{0}")]
    Skill(String),
    /// Generic message captured during the migration from
    /// `Result<T, String>`. New code should prefer one of the categorical
    /// variants above; this exists so legacy `format!(...)` strings can
    /// flow through `?` without losing their wording.
    #[error("{0}")]
    Other(String),
}

impl PackageError {
    pub fn message(&self) -> &str {
        match self {
            PackageError::Manifest(s)
            | PackageError::Lockfile(s)
            | PackageError::Registry(s)
            | PackageError::Validation(s)
            | PackageError::Ops(s)
            | PackageError::Extensions(s)
            | PackageError::Skill(s)
            | PackageError::Other(s) => s.as_str(),
        }
    }
}

impl From<String> for PackageError {
    fn from(value: String) -> Self {
        PackageError::Other(value)
    }
}

impl From<&str> for PackageError {
    fn from(value: &str) -> Self {
        PackageError::Other(value.to_string())
    }
}

/// Migration bridge: lets callers that still return `Result<T, String>`
/// receive a `PackageError` via `?` without losing the user-facing
/// message. Once all CLI command modules adopt typed errors this can be
/// removed.
impl From<PackageError> for String {
    fn from(error: PackageError) -> Self {
        error.to_string()
    }
}

#[allow(dead_code)]
pub type PackageResult<T> = StdResult<T, PackageError>;
