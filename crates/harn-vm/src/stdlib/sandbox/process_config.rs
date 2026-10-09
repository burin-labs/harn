use std::path::{Path, PathBuf};

use crate::orchestration::CapabilityPolicy;
use crate::security::environment_policy::environment_names_equal;
use crate::security::session_environment::insert_env_value;

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
    if rustc_wrapper::probing() {
        return;
    }
    if let Some((policy, _)) = super::active_sandbox_policy() {
        decide_and_apply(&policy, cwd, env, env_remove);
    }
}

/// Apply wrapper policy only when launching Cargo directly. Other programs,
/// including shells that might start Cargo later, receive empty wrapper
/// settings to clear inherited and Cargo-configured wrappers. A shell can set
/// them again; nested launches remain governed by the OS sandbox.
/// This keeps unrelated commands off the compiler probe's startup path.
pub fn apply_active_rustc_wrapper_policy_for_command(
    program: &str,
    args: &[String],
    command: &mut std::process::Command,
    closed_env: bool,
) {
    if rustc_wrapper::probing() {
        return;
    }
    if let Some((policy, _)) = super::active_sandbox_policy() {
        if !cargo_may_compile(program, args) {
            for key in RUSTC_WRAPPER_ENV_KEYS {
                command.env(key, "");
            }
            return;
        }
        let mut env: std::collections::BTreeMap<_, _> = if closed_env {
            Default::default()
        } else {
            std::env::vars().collect()
        };
        for (key, value) in command.get_envs() {
            let key = key.to_string_lossy().into_owned();
            if let Some(value) = value {
                insert_env_value(&mut env, &key, value.to_string_lossy().into_owned());
            } else {
                env.retain(|name, _| !environment_names_equal(name, &key));
            }
        }
        let cwd = command
            .get_current_dir()
            .map(Path::to_path_buf)
            .or_else(|| super::policy_process_cwd(&policy, None).ok());
        let disable = cwd.is_none_or(|cwd| {
            rustc_wrapper::rustc_wrapper_decision_for_environment(&policy, &cwd, &env).disables()
        });
        if disable {
            for key in RUSTC_WRAPPER_ENV_KEYS {
                command.env(key, "");
            }
        }
    }
}

fn cargo_may_compile(program: &str, args: &[String]) -> bool {
    let cargo = Path::new(program).file_name().is_some_and(|name| {
        name == "cargo"
            || (cfg!(windows)
                && name
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("cargo.exe")))
    });
    // Cargo's informational commands never invoke a compiler wrapper. Unknown
    // commands may be plugins that compile, so keep the probe for those.
    if !cargo {
        return false;
    }
    let mut args = args.iter().map(String::as_str).peekable();
    if args.peek().is_some_and(|arg| arg.starts_with('+')) {
        args.next();
    }
    while let Some(arg) = args.next() {
        match arg {
            "--offline" | "--locked" | "--frozen" | "-v" | "-vv" | "--verbose" | "-q"
            | "--quiet" => continue,
            "--color" | "--config" | "-C" | "-Z" => {
                args.next();
                continue;
            }
            value
                if value.starts_with("--color=")
                    || value.starts_with("--config=")
                    || value.starts_with("-Z") =>
            {
                continue
            }
            command => {
                return !matches!(
                    command,
                    "--version"
                        | "-V"
                        | "--help"
                        | "-h"
                        | "--list"
                        | "help"
                        | "metadata"
                        | "locate-project"
                        | "verify-project"
                        | "fetch"
                        | "generate-lockfile"
                        | "update"
                        | "search"
                        | "info"
                        | "tree"
                        | "clean"
                )
            }
        }
    }
    false
}

/// [`apply_active_rustc_wrapper_policy`] for a config already known to run
/// under `policy`.
pub(super) fn apply_rustc_wrapper_decision(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    config: &mut ProcessCommandConfig,
) {
    if rustc_wrapper::probing() {
        return;
    }
    if !cargo_may_compile(program, args) {
        neutralize_rustc_wrapper(&mut config.env, &mut config.env_remove);
        return;
    }
    let mut env: std::collections::BTreeMap<_, _> = if config.closed_env {
        Default::default()
    } else {
        std::env::vars().collect()
    };
    for (key, value) in &config.env {
        insert_env_value(&mut env, key, value.clone());
    }
    for key in &config.env_remove {
        env.retain(|name, _| !environment_names_equal(name, key));
    }
    if config.cwd.as_ref().is_none_or(|cwd| {
        rustc_wrapper::rustc_wrapper_decision_for_environment(policy, cwd, &env).disables()
    }) {
        neutralize_rustc_wrapper(&mut config.env, &mut config.env_remove);
    }
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
        env.retain(|(existing, _)| !environment_names_equal(existing, key));
        env_remove.retain(|removed| !environment_names_equal(removed, key));
        env.push((key.to_string(), String::new()));
    }
}

pub(super) fn sandboxed_process_config(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> Result<ProcessCommandConfig, crate::VmError> {
    let mut resolved = config.clone();
    if let Some(cwd) = resolved.cwd.as_ref() {
        super::enforce_process_cwd_for_policy(cwd, policy)?;
    } else {
        resolved.cwd = Some(super::policy_process_cwd(policy, None)?);
    }
    super::inject_workspace_process_env(&mut resolved.env, policy);
    apply_rustc_wrapper_decision(program, args, policy, &mut resolved);
    resolved.env.retain(|(key, _)| {
        !resolved
            .env_remove
            .iter()
            .any(|removed| environment_names_equal(key, removed))
    });
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::cargo_may_compile;

    #[test]
    fn cargo_global_options_and_toolchain_preserve_command_classification() {
        for args in [
            vec!["+stable", "--offline", "--version"],
            vec!["--config", "build.jobs=1", "--color=never", "metadata"],
            vec!["+stable", "--help"],
        ] {
            assert!(!cargo_may_compile(
                "cargo",
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>()
            ));
        }
        for args in [
            vec!["+stable", "--offline", "build"],
            vec!["--config", "build.jobs=1", "custom-plugin"],
        ] {
            assert!(cargo_may_compile(
                "cargo",
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>()
            ));
        }
    }
}
