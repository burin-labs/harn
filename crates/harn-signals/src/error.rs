use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignalsError {
    #[error("signals: cannot find a home directory; set HARN_EXT_SIGNALS_DIR")]
    NoHome,
    #[error("signals: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("signals: {0}")]
    Lock(#[from] harn_flock::LockError),
    #[error("signals: inbox {path} is unreadable: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("signals: config {path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("signals: invalid triage event: {0}")]
    InvalidEvent(String),
    #[error("signals: {0}")]
    InvalidInput(String),
    #[error("signals: the inbox already holds {0} fired signals (max_signals); resolve some or raise the cap")]
    Full(usize),
    #[error("signals: `{key}` was reported with a {stored} signature; a report with a {offered} one cannot change it")]
    WeakerSignature {
        key: String,
        stored: &'static str,
        offered: &'static str,
    },
}

impl SignalsError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}
