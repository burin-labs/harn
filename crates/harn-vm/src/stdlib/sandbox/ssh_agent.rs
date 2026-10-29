#[cfg(target_os = "linux")]
use crate::stdlib::sandbox;
use crate::stdlib::sandbox::{sandbox_rejection, CapabilityPolicy};
use crate::VmError;

pub(in crate::stdlib::sandbox) fn validate(policy: &CapabilityPolicy) -> Result<(), VmError> {
    let Some(socket) = &policy.process_sandbox.ssh_agent_socket else {
        return Ok(());
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::fs::FileTypeExt;
        let path = std::path::Path::new(socket);
        if !path.is_absolute()
            || !std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            return Err(sandbox_rejection(
                "SSH agent grant requires an existing absolute Unix socket path".into(),
            ));
        }
        #[cfg(target_os = "linux")]
        if !sandbox::policy_allows_network(policy)
            && !sandbox::linux::ssh_agent_path_scope_available()
        {
            return Err(sandbox_rejection(
                "SSH agent socket connection requires Landlock ABI 9 with measured enforcement on this host".into(),
            ));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = socket;
        Err(sandbox_rejection(
            "this process sandbox cannot enforce an SSH agent socket grant".into(),
        ))
    }
}
