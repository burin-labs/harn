use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;

/// Whether a Cargo `rustc` wrapper runs inside the sandbox, and the receipt.
#[path = "rustc_wrapper.rs"]
pub mod rustc_wrapper;
use rustc_wrapper::RUSTC_WRAPPER_ENV_KEYS;

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

/// Apply the recorded wrapper decision to a spawn governed by an active sandbox.
///
/// A wrapper that runs under the active profile is left in place; one that
/// cannot, or that would leave a confined long-lived process behind, is
/// switched off by setting every wrapper key empty, which overrides Cargo
/// configuration files too. Unsandboxed spawns are left unchanged. The
/// decision is measured once per (policy, working directory) and recorded in
/// [`rustc_wrapper::rustc_wrapper_decisions`].
pub fn apply_active_rustc_wrapper_policy(
    env: &mut Vec<(String, String)>,
    env_remove: &mut Vec<String>,
    cwd: Option<&Path>,
) {
    if let Some((policy, _)) = super::active_sandbox_policy() {
        decide_and_apply(&policy, cwd, env, env_remove);
    }
}

/// [`apply_active_rustc_wrapper_policy`] for a config already known to run
/// under `policy`.
pub(super) fn apply_rustc_wrapper_decision(
    policy: &CapabilityPolicy,
    config: &mut ProcessCommandConfig,
) {
    let cwd = config.cwd.clone();
    decide_and_apply(
        policy,
        cwd.as_deref(),
        &mut config.env,
        &mut config.env_remove,
    );
}

fn decide_and_apply(
    policy: &CapabilityPolicy,
    cwd: Option<&Path>,
    env: &mut Vec<(String, String)>,
    env_remove: &mut Vec<String>,
) {
    if rustc_wrapper::probing() {
        return;
    }
    let cwd = match cwd {
        Some(cwd) => cwd.to_path_buf(),
        None => match super::policy_process_cwd(policy, None) {
            Ok(cwd) => cwd,
            Err(_) => return neutralize_rustc_wrapper(env, env_remove),
        },
    };
    if rustc_wrapper::rustc_wrapper_decision(policy, &cwd, env).disables() {
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
