use std::process::{Command, Stdio};

use super::ProcessCommandConfig;
use crate::orchestration::{CapabilityPolicy, ProcessNetworkProxy};

/// Environment overlay that pins child-tool diagnostics to deterministic,
/// English, UTF-8-preserving messages without changing the character locale.
pub fn deterministic_message_locale_env() -> Vec<(String, String)> {
    vec![
        ("LC_MESSAGES".to_string(), "C".to_string()),
        ("DOTNET_CLI_UI_LANGUAGE".to_string(), "en".to_string()),
    ]
}

/// A user-inherited value here overrides `LC_MESSAGES`, so spawn sites remove
/// it unless the caller explicitly pinned it.
pub const MESSAGE_LOCALE_OVERRIDE_ENV: &str = "LC_ALL";

pub(super) fn apply_process_config(
    command: &mut Command,
    config: &ProcessCommandConfig,
    policy: Option<&CapabilityPolicy>,
) {
    if let Some(cwd) = config.cwd.as_ref() {
        command.current_dir(cwd);
    }
    if config.closed_env {
        command.env_clear();
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    for key in &config.env_remove {
        command.env_remove(key);
    }
    if let Some(proxy) = policy.and_then(|policy| policy.process_network_proxy) {
        apply_managed_proxy_env(command, proxy);
    }
    if config.stdin_null {
        command.stdin(Stdio::null());
    }
}

pub(super) fn apply_managed_proxy_env(command: &mut Command, proxy: ProcessNetworkProxy) {
    for (key, value) in managed_proxy_environment(proxy) {
        command.env(key, value);
    }
}

pub(super) fn apply_managed_proxy_env_tokio(
    command: &mut tokio::process::Command,
    proxy: ProcessNetworkProxy,
) {
    for (key, value) in managed_proxy_environment(proxy) {
        command.env(key, value);
    }
}

pub(super) fn managed_proxy_environment(proxy: ProcessNetworkProxy) -> Vec<(&'static str, String)> {
    let http = format!("http://127.0.0.1:{}", proxy.http_port);
    let socks = format!("socks5h://127.0.0.1:{}", proxy.socks_port);
    let mut env = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "NPM_CONFIG_PROXY",
        "NPM_CONFIG_HTTP_PROXY",
        "NPM_CONFIG_HTTPS_PROXY",
        "YARN_HTTP_PROXY",
        "YARN_HTTPS_PROXY",
        "PIP_PROXY",
    ]
    .into_iter()
    .map(|key| (key, http.clone()))
    .collect::<Vec<_>>();
    env.extend(
        ["ALL_PROXY", "all_proxy"]
            .into_iter()
            .map(|key| (key, socks.clone())),
    );
    // A launcher-level bypass must not let an allowlisted hostname resolve to
    // a direct child socket. The kernel boundary would deny it anyway, but
    // clearing this makes cooperative tools reach the managed proxy.
    env.extend([
        ("NO_PROXY", String::new()),
        ("no_proxy", String::new()),
        ("HARN_PROCESS_EGRESS_PROXY", "managed".to_string()),
    ]);
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn every_process_launch_surface_rejects_managed_proxy_without_kernel_boundary() {
        let policy = CapabilityPolicy {
            process_network_proxy: Some(ProcessNetworkProxy {
                http_port: 3128,
                socks_port: 1080,
            }),
            ..Default::default()
        };

        let error = super::super::ensure_managed_process_egress_supported::<
            super::super::ActiveBackend,
        >(&policy)
        .unwrap_err();
        assert!(error.to_string().contains("not enforceable"), "{error}");
    }

    #[test]
    fn managed_proxy_environment_wins_over_caller_bypass_values() {
        let mut command = Command::new("ignored");
        let config = ProcessCommandConfig {
            env: vec![
                (
                    "HTTP_PROXY".to_string(),
                    "http://attacker.invalid".to_string(),
                ),
                ("NO_PROXY".to_string(), "*".to_string()),
            ],
            ..Default::default()
        };
        let policy = CapabilityPolicy {
            process_network_proxy: Some(ProcessNetworkProxy {
                http_port: 3128,
                socks_port: 1080,
            }),
            ..Default::default()
        };

        apply_process_config(&mut command, &config, Some(&policy));
        let env = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_string_lossy().to_string(),
                        value.to_string_lossy().to_string(),
                    )
                })
            })
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(env["HTTP_PROXY"], "http://127.0.0.1:3128");
        assert_eq!(env["ALL_PROXY"], "socks5h://127.0.0.1:1080");
        assert_eq!(env["NO_PROXY"], "");
    }
}

/// Run the Windows AppContainer output path without blocking a Tokio worker.
///
/// Windows cannot attach an AppContainer to `tokio::process::Command`, so an
/// async capability call captures its policy and closed environment before
/// moving the custom `CreateProcessW` launch to the blocking pool.
#[cfg(target_os = "windows")]
pub(crate) async fn windows_command_output(
    program: String,
    args: Vec<String>,
    config: ProcessCommandConfig,
    policy: crate::orchestration::CapabilityPolicy,
    profile: crate::orchestration::SandboxProfile,
) -> Result<std::process::Output, crate::value::VmError> {
    let config = if config.closed_env {
        config
    } else if let Some(env) = crate::stdlib::process::session_closed_env_for_command(
        &program,
        config.env.iter().cloned(),
    )? {
        ProcessCommandConfig {
            env,
            closed_env: true,
            ..config
        }
    } else {
        config
    };
    let config = super::sandboxed_process_config(&config, &policy)?;
    let output = tokio::task::spawn_blocking(move || {
        <super::ActiveBackend as super::SandboxBackend>::run_to_output(
            &program, &args, &config, &policy, profile,
        )
    })
    .await
    .map_err(|error| {
        crate::value::VmError::Runtime(format!("Windows process worker failed: {error}"))
    })??;
    let refusal_command: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect();
    let refusal_cwd = config
        .cwd
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    if let Some(error) = super::process_violation_error(&output, &refusal_command, &refusal_cwd) {
        return Err(error);
    }
    Ok(output)
}
