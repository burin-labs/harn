use std::path::PathBuf;

/// Exact bytes supplied to a child process.
///
/// `Null` means no input was requested, while `Bytes(Vec::new())` preserves
/// the distinct request to open and immediately close an empty input stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProcessStdin {
    #[default]
    Null,
    Bytes(Vec<u8>),
}

/// Process launch settings normalized before platform-specific dispatch.
#[derive(Clone, Debug, Default)]
pub struct ProcessCommandConfig {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Environment keys removed after the inherited/session environment and
    /// caller overlays have been composed.
    pub env_remove: Vec<String>,
    pub stdin: ProcessStdin,
    /// When `true`, the child starts from an EMPTY environment and receives only
    /// the pairs in [`ProcessCommandConfig::env`]. The active session environment
    /// has already composed the policy snapshot and grants into this list.
    pub closed_env: bool,
}
