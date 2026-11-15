use std::process::{Command, Stdio};

use super::ProcessCommandConfig;
use crate::orchestration::CapabilityPolicy;

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
    } else {
        command.envs(crate::stdlib::process::granted_ssh_agent_env());
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    for key in &config.env_remove {
        command.env_remove(key);
    }
    if let Some(policy) = policy {
        apply_managed_proxy_env(command, policy);
    }
    match &config.stdin {
        super::ProcessStdin::Null => {
            command.stdin(Stdio::null());
        }
        super::ProcessStdin::Bytes(_) => {
            command.stdin(Stdio::piped());
        }
    }
}

pub(super) fn apply_managed_proxy_env(command: &mut Command, policy: &CapabilityPolicy) {
    for (key, value) in managed_proxy_environment(policy) {
        command.env(key, value);
    }
}

pub(super) fn apply_managed_proxy_env_tokio(
    command: &mut tokio::process::Command,
    policy: &CapabilityPolicy,
) {
    for (key, value) in managed_proxy_environment(policy) {
        command.env(key, value);
    }
}

/// What a child behind the managed proxy is told about it. Empty without one.
pub(super) fn managed_proxy_environment(policy: &CapabilityPolicy) -> Vec<(&'static str, String)> {
    let Some(proxy) = policy.process_network_proxy else {
        return Vec::new();
    };
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
    // clearing this makes cooperative tools reach the managed proxy. A command
    // with a loopback grant reaches its own local servers directly, which the
    // proxy would refuse, so only the loopback names bypass it.
    let bypass = if super::policy::ProcessNetworkGrant::for_policy(policy).loopback_behind_proxy() {
        "localhost,127.0.0.1,::1"
    } else {
        ""
    };
    env.extend([
        ("NO_PROXY", bypass.to_string()),
        ("no_proxy", bypass.to_string()),
        ("HARN_PROCESS_EGRESS_PROXY", "managed".to_string()),
    ]);
    // Marks the processes this runtime started, so a later spawn can tell
    // their local listeners from other services' (see `macos/own_listeners.rs`).
    env.push((CHILD_LINEAGE_ENV, child_lineage().to_string()));
    env
}

/// Environment name of the marker every child behind the managed proxy carries.
pub(crate) const CHILD_LINEAGE_ENV: &str = "HARN_CHILD_LINEAGE";

/// This runtime's marker value. It is random per runtime, so another
/// runtime's children do not carry it by accident. It is a label, not a
/// secret: any process of the same user can read a child's environment and
/// copy it.
pub(crate) fn child_lineage() -> &'static str {
    static LINEAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LINEAGE.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ProcessNetworkProxy;

    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn every_process_launch_surface_rejects_managed_proxy_without_kernel_boundary() {
        let policy = CapabilityPolicy {
            process_network_proxy: Some(ProcessNetworkProxy {
                http_port: 3128,
                socks_port: 1080,
            }),
            ..Default::default()
        };

        let error = super::super::ensure_spawn_enforceable::<super::super::ActiveBackend>(&policy)
            .unwrap_err();
        assert!(error.to_string().contains("not enforceable"), "{error}");
    }

    #[test]
    fn managed_proxy_environment_wins_over_caller_bypass_values() {
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

        let env = |policy: &CapabilityPolicy| {
            let mut command = Command::new("ignored");
            apply_process_config(&mut command, &config, Some(policy));
            command
                .get_envs()
                .filter_map(|(key, value)| {
                    value.map(|value| {
                        (
                            key.to_string_lossy().to_string(),
                            value.to_string_lossy().to_string(),
                        )
                    })
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };

        let public = env(&policy);
        assert_eq!(public["HTTP_PROXY"], "http://127.0.0.1:3128");
        assert_eq!(public["ALL_PROXY"], "socks5h://127.0.0.1:1080");
        assert_eq!(public["NO_PROXY"], "");
        assert_eq!(public[CHILD_LINEAGE_ENV], child_lineage());

        // A loopback grant reaches its own local servers directly; everything
        // else still goes through the proxy.
        let mut loopback = policy;
        loopback.process_sandbox.allow_tcp_loopback = true;
        let loopback = env(&loopback);
        assert_eq!(loopback["HTTP_PROXY"], "http://127.0.0.1:3128");
        assert_eq!(loopback["NO_PROXY"], "localhost,127.0.0.1,::1");
        assert_eq!(loopback["no_proxy"], "localhost,127.0.0.1,::1");
    }
}
