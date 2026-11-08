//! A moved replay is a confined filesystem view, never just rewritten text.

use std::path::Path;

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::VmError;

fn roots() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let branch = crate::run_tape::state_branch()?;
    branch.relocated().then(|| {
        (
            branch
                .recorded_workspace()
                .expect("relocated branch")
                .to_path_buf(),
            branch.workspace().to_path_buf(),
        )
    })
}

/// Narrow the existing child policy. Explicit host grants are projected;
/// ambient mounts, package configuration and shared temporary trees are not.
pub(super) fn policy(mut policy: CapabilityPolicy) -> CapabilityPolicy {
    let Some((_, workspace)) = roots() else {
        return policy;
    };
    policy.sandbox_profile = SandboxProfile::OsHardened;
    policy.workspace_roots = vec![workspace.display().to_string()];
    let projection = crate::run_tape::workspace::current_projection().expect("branch projection");
    let process = policy.process_sandbox.as_mut();
    for roots in [
        &mut policy.read_only_roots,
        &mut process.read_roots,
        &mut process.write_roots,
        &mut process.unix_socket_roots,
    ] {
        for root in roots {
            *root = projection.text(root);
        }
    }
    if let Some(socket) = &mut process.ssh_agent_socket {
        *socket = projection.text(socket);
    }
    let mapped_denials: Vec<_> = process
        .read_deny_roots
        .iter()
        .map(|root| projection.text(root))
        .collect();
    for root in mapped_denials {
        if !process.read_deny_roots.contains(&root) {
            process.read_deny_roots.push(root);
        }
    }
    policy.process_sandbox.presets = Some(
        policy
            .process_sandbox
            .effective_presets()
            .into_iter()
            .filter(|p| {
                matches!(
                    p,
                    ProcessSandboxPreset::SystemRuntime | ProcessSandboxPreset::DeveloperToolchains
                )
            })
            .collect(),
    );
    policy
}

pub(crate) fn check_supported() -> Result<(), VmError> {
    if roots().is_none() {
        return Ok(());
    }
    // Other renderers retain the refusal until their complete grant set is
    // checked and the opaque-child negative control proves confinement.
    if !cfg!(target_os = "linux")
        || super::active_sandbox_policy().is_none()
        || !super::active_backend_filesystem_available()
    {
        return Err(super::sandbox_rejection(
            "live commands in a relocated replay workspace require an enforced filesystem namespace on this host".into(),
        ));
    }
    Ok(())
}

/// Check the final, normalized grants the OS renderer actually installs.
/// Presets and executable/PATH grants must not reopen the recorded tree or
/// the container holding this arm. Other arms in that container therefore
/// stay outside the view, including ones created after this child starts.
pub(super) fn check_grant(path: &Path) -> Result<(), VmError> {
    let Some((recorded, workspace)) = roots() else {
        return Ok(());
    };
    let branch = crate::run_tape::state_branch().expect("relocated branch");
    let container = branch.workspace_container();
    let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    if overlaps(path, &recorded) || (!path.starts_with(&workspace) && overlaps(path, container)) {
        return Err(super::sandbox_rejection(format!(
            "replay filesystem namespace cannot admit grant '{}' outside its isolated workspace",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn workspace_roots() -> Option<Vec<std::path::PathBuf>> {
    roots().map(|(_, workspace)| vec![workspace])
}

pub(super) fn project_environment(
    command: &mut std::process::Command,
    closed: bool,
) -> Result<(), VmError> {
    let Some((_, workspace)) = roots() else {
        return Ok(());
    };
    let projection = crate::run_tape::workspace::current_projection().expect("branch projection");
    let mut environment: std::collections::BTreeMap<_, _> = (!closed)
        .then(std::env::vars_os)
        .into_iter()
        .flatten()
        .collect();
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => {
                environment.insert(name.to_owned(), value.to_owned());
            }
            None => {
                environment.remove(name);
            }
        }
    }
    for (name, value) in environment {
        if let Some(text) = value.to_str() {
            let mapped = projection.text(text);
            if mapped != text {
                command.env(name, mapped);
            }
        }
    }
    let cwd = command
        .get_current_dir()
        .map(|p| projection.path(p))
        .unwrap_or(workspace.clone());
    let cwd = super::normalize_for_policy(&cwd);
    if !cwd.starts_with(&workspace) {
        return Err(super::sandbox_rejection(
            "replay command cwd is outside its isolated workspace".into(),
        ));
    }
    command.current_dir(cwd);
    Ok(())
}
