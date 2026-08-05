//! Typed error enum for the `orchestrator` CLI module.
//!
//! Variants preserve exact user-facing message text via `Display`, keeping
//! the CLI's printed output byte-for-byte identical to the prior
//! `Result<T, String>` API while giving library callers a stable type.

use std::result::Result as StdResult;

use crate::package::errors::PackageError;

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("{0}")]
    Common(String),
    #[error("{0}")]
    Listener(String),
    #[error("{0}")]
    Serve(String),
    #[error("{0}")]
    Deploy(String),
    #[error("{0}")]
    Inspect(String),
    #[error("{0}")]
    Stats(String),
    #[error("{0}")]
    Reload(String),
    #[error("{0}")]
    Recover(String),
    #[error("{0}")]
    Tenant(String),
    #[error("{0}")]
    Tls(String),
    #[error("{0}")]
    Queue(String),
    #[error("{0}")]
    Dlq(String),
    #[error("{0}")]
    Fire(String),
    #[error("{0}")]
    Replay(String),
    #[error("{0}")]
    Resume(String),
    #[error("{0}")]
    Role(String),
    /// Generic message captured during the `Result<T, String>` migration.
    /// New code should prefer one of the categorical variants above.
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Package(#[from] PackageError),
}

impl OrchestratorError {
    pub fn message(&self) -> String {
        match self {
            OrchestratorError::Common(s)
            | OrchestratorError::Listener(s)
            | OrchestratorError::Serve(s)
            | OrchestratorError::Deploy(s)
            | OrchestratorError::Inspect(s)
            | OrchestratorError::Stats(s)
            | OrchestratorError::Reload(s)
            | OrchestratorError::Recover(s)
            | OrchestratorError::Tenant(s)
            | OrchestratorError::Tls(s)
            | OrchestratorError::Queue(s)
            | OrchestratorError::Dlq(s)
            | OrchestratorError::Fire(s)
            | OrchestratorError::Replay(s)
            | OrchestratorError::Resume(s)
            | OrchestratorError::Role(s)
            | OrchestratorError::Other(s) => s.clone(),
            OrchestratorError::Package(err) => err.to_string(),
        }
    }
}

impl From<String> for OrchestratorError {
    fn from(value: String) -> Self {
        OrchestratorError::Other(value)
    }
}

impl From<&str> for OrchestratorError {
    fn from(value: &str) -> Self {
        OrchestratorError::Other(value.to_string())
    }
}

/// Migration bridge: lets callers that still return `Result<T, String>`
/// receive an `OrchestratorError` via `?` without losing the user-facing
/// message. Once all CLI command modules adopt typed errors this can be
/// removed.
impl From<OrchestratorError> for String {
    fn from(error: OrchestratorError) -> Self {
        error.to_string()
    }
}

pub type OrchestratorResult<T> = StdResult<T, OrchestratorError>;
