use std::process::{Command, Stdio};

use super::ProcessCommandConfig;

pub(super) fn apply_process_config(command: &mut Command, config: &ProcessCommandConfig) {
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
    if config.stdin_null {
        command.stdin(Stdio::null());
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
    if let Some(error) = super::process_violation_error(&output) {
        return Err(error);
    }
    Ok(output)
}
