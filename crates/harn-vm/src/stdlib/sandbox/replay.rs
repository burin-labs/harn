//! A moved replay is a confined filesystem view, never just rewritten text.

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::value::{ErrorCategory, VmError, VmValue};

pub(super) fn relocation_unconfined(message: &str) -> VmError {
    VmError::Thrown(VmValue::dict([
        (
            "category",
            VmValue::string(ErrorCategory::ToolRejected.as_str()),
        ),
        (
            "reason_code",
            VmValue::string("replay_relocation_unconfined"),
        ),
        ("message", VmValue::string(message)),
    ]))
}

pub(super) struct FilesystemBoundary {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub recorded: Vec<std::path::PathBuf>,
    pub workspace: std::path::PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub container: std::path::PathBuf,
}

pub(super) fn boundary() -> Option<FilesystemBoundary> {
    let branch = crate::run_tape::state_branch()?;
    branch.relocated().then(|| FilesystemBoundary {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        recorded: branch
            .recorded_process_roots()
            .into_iter()
            .filter(|root| root.as_path() != branch.workspace())
            .collect(),
        workspace: branch.workspace().to_path_buf(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        container: branch.workspace_container().to_path_buf(),
    })
}

/// Narrow the existing child policy. Explicit host grants are projected;
/// ambient mounts, package configuration and shared temporary trees are not.
pub(super) fn policy(mut policy: CapabilityPolicy) -> CapabilityPolicy {
    let Some(FilesystemBoundary { workspace, .. }) = boundary() else {
        return policy;
    };
    policy.sandbox_profile = SandboxProfile::OsHardened;
    policy.workspace_roots = vec![workspace.display().to_string()];
    let projection = crate::run_tape::workspace::physical_projection().expect("branch projection");
    let project_root = |root: &str| {
        let path = super::paths::lexical_for_policy(std::path::Path::new(root));
        // Retained aliases remain meaningful after unlinking. Resolve those
        // before canonicalizing, then project canonical filesystem aliases.
        let named = projection.text(&path.to_string_lossy());
        projection.text(&super::render_policy_root(&named).to_string_lossy())
    };
    let process = policy.process_sandbox.as_mut();
    for roots in [
        &mut policy.read_only_roots,
        &mut process.read_roots,
        &mut process.write_roots,
        &mut process.unix_socket_roots,
    ] {
        for root in roots {
            *root = project_root(root);
        }
    }
    if let Some(socket) = &mut process.ssh_agent_socket {
        *socket = project_root(socket);
    }
    let mapped_denials: Vec<_> = process
        .read_deny_roots
        .iter()
        .map(|root| project_root(root))
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

/// Admission has no active command policy yet; check platform support only.
pub(crate) fn check_platform_supported() -> Result<(), VmError> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Err(relocation_unconfined(
            "cannot replay commands in a moved workspace on this host; an enforced filesystem namespace is required",
        ));
    }
    #[cfg(target_os = "macos")]
    super::macos::check_replay_supported()?;
    Ok(())
}

pub(crate) fn check_supported() -> Result<(), VmError> {
    if boundary().is_none() {
        return Ok(());
    }
    check_platform_supported()?;
    if super::active_sandbox_policy().is_none() || !super::active_backend_filesystem_available() {
        return Err(relocation_unconfined(
            "live commands in a relocated replay workspace require an enforced filesystem namespace on this host",
        ));
    }
    Ok(())
}

/// Check the final, normalized grants the OS renderer actually installs.
/// Presets and executable/PATH grants must not reopen the recorded tree or
/// the container holding this arm. Other arms in that container therefore
/// stay outside the view, including ones created after this child starts.
#[cfg(target_os = "linux")]
pub(super) fn check_grant(path: &std::path::Path) -> Result<(), VmError> {
    let Some(FilesystemBoundary {
        recorded,
        workspace,
        container,
    }) = boundary()
    else {
        return Ok(());
    };
    let overlaps = |a: &std::path::Path, b: &std::path::Path| a.starts_with(b) || b.starts_with(a);
    if recorded.iter().any(|recorded| overlaps(path, recorded))
        || (!path.starts_with(&workspace) && overlaps(path, &container))
    {
        return Err(super::sandbox_rejection(format!(
            "replay filesystem namespace cannot admit grant '{}' outside its isolated workspace",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn workspace_roots() -> Option<Vec<std::path::PathBuf>> {
    boundary().map(|boundary| vec![boundary.workspace])
}

pub(super) fn bind_command_cwd(command: &mut std::process::Command) -> Result<(), VmError> {
    let Some(FilesystemBoundary { workspace, .. }) = boundary() else {
        return Ok(());
    };
    let cwd = command
        .get_current_dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or(workspace.clone());
    let cwd = crate::run_tape::workspace_path(cwd)?;
    let cwd = super::normalize_for_policy(&cwd);
    if !cwd.starts_with(&workspace) {
        return Err(super::sandbox_rejection(
            "replay command cwd is outside its isolated workspace".into(),
        ));
    }
    command.current_dir(cwd);
    Ok(())
}
