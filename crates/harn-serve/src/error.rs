use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    Unauthorized(String),
    Validation(String),
    MissingExport(String),
    Cancelled(String),
    Execution(String),
    Io(String),
    Cache(String),
}

impl Display for DispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized(message)
            | Self::Validation(message)
            | Self::MissingExport(message)
            | Self::Cancelled(message)
            | Self::Execution(message)
            | Self::Io(message)
            | Self::Cache(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DispatchError {}
