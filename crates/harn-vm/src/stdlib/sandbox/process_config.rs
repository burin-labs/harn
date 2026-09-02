use std::path::PathBuf;

const RUSTC_WRAPPER_ENV_KEYS: [&str; 4] = [
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

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

/// Disable Cargo `rustc` and workspace wrappers for a spawn governed by an active sandbox.
///
/// A wrapper such as `sccache` is a shared per-user daemon. If a sandboxed
/// Cargo invocation starts it, the daemon inherits that confinement and can
/// poison later builds outside the workspace. Empty wrapper values override
/// Cargo configuration while leaving unsandboxed builds and caches unchanged.
pub fn apply_active_rustc_wrapper_policy(
    env: &mut Vec<(String, String)>,
    env_remove: &mut Vec<String>,
) {
    if super::active_sandbox_policy().is_some() {
        neutralize_rustc_wrapper(env, env_remove);
    }
}

pub(super) fn neutralize_rustc_wrapper(
    env: &mut Vec<(String, String)>,
    env_remove: &mut Vec<String>,
) {
    for key in RUSTC_WRAPPER_ENV_KEYS {
        env.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        env_remove.retain(|removed| !removed.eq_ignore_ascii_case(key));
        env.push((key.to_string(), String::new()));
    }
}
