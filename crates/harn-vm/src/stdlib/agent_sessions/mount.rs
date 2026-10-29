//! Mounting a root on a session through the sandbox that will confine its
//! children.

use crate::agent_sessions;

/// Mount a root on a session, refusing a `read_only` mount the session's
/// sandbox could not hold. Every host entry point mounts through this, not
/// through [`agent_sessions::add_workspace_root`].
///
/// `policy` is the capability policy the session's confined children run
/// under, for a host that mounts outside a running prompt (an idle session has
/// no current execution policy). `None` measures against the current one.
pub fn add_workspace_root(
    id: &str,
    root: &str,
    mount_mode: Option<crate::workspace_anchor::MountMode>,
    reason: Option<String>,
    policy: Option<&crate::orchestration::CapabilityPolicy>,
) -> Result<String, String> {
    agent_sessions::add_workspace_root_verified(id, root, mount_mode, reason, |mounted| {
        refuse_unenforceable_read_only(id, mounted, policy)
    })
}

/// Persist a host's standalone mutation before acknowledging the grant.
pub async fn add_workspace_root_persisted(
    id: &str,
    root: &str,
    mount_mode: Option<crate::workspace_anchor::MountMode>,
    reason: Option<String>,
    policy: Option<&crate::orchestration::CapabilityPolicy>,
    scope: &crate::SessionStoreScope,
) -> Result<String, String> {
    agent_sessions::add_workspace_root_verified_persisted(
        id,
        root,
        mount_mode,
        reason,
        scope,
        |mounted| refuse_unenforceable_read_only(id, mounted, policy),
    )
    .await
}

/// Refuse a `read_only` mount when the active
/// policy's confined child would still be able to write it.
///
/// Landlock rules only add access, so a child keeps its parent root's write
/// grant over the nested mount; the macOS profile re-denies it. A mount the
/// kernel would not hold must not be reported as read-only. Only a measured
/// `Writable` refuses: an unconfined or unmeasured child is left to the
/// approval boundary, as for any other read-only root.
fn refuse_unenforceable_read_only(
    id: &str,
    root: &std::path::Path,
    policy: Option<&crate::orchestration::CapabilityPolicy>,
) -> Result<(), String> {
    use crate::workspace_anchor::MountMode;
    let Some(anchor) = agent_sessions::workspace_anchor(id) else {
        return Ok(());
    };
    let mounted_read_only = anchor
        .additional_roots
        .iter()
        .any(|entry| entry.path == root && matches!(entry.mount_mode, MountMode::ReadOnly));
    if !mounted_read_only {
        return Ok(());
    }
    let Some(policy) = policy
        .cloned()
        .or_else(crate::orchestration::current_execution_policy)
    else {
        return Ok(());
    };
    // The sandbox projection reads the current session's mounts, so measure
    // inside this session's scope whichever session, if any, is running.
    let _session = agent_sessions::enter_current_session(id);
    if crate::stdlib::sandbox::child_write::child_write_disposition(&policy, root)
        == crate::stdlib::sandbox::child_write::ChildWriteDisposition::Writable
    {
        return Err(format!(
            "cannot mount '{}' read_only: this platform's sandbox cannot subtract write \
             access from a path inside a writable root, so a command could still write it",
            root.display()
        ));
    }
    Ok(())
}
