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
        refuse_overlapping_mount(id, mounted, policy)?;
        refuse_unenforceable_read_only(id, mounted, policy)
    })
}

/// Refuse a mount that would leave a writable root inside a read-only one.
///
/// A `read_only` mount promises that nothing under it can be written, and
/// neither the file builtins nor either sandbox backend subtracts a writable
/// root nested inside it. So a read-only mount that contains the anchor
/// primary, a writable mount, or a policy workspace root is refused, and so is
/// a writable mount placed inside an existing read-only one.
fn refuse_overlapping_mount(
    id: &str,
    root: &std::path::Path,
    policy: Option<&crate::orchestration::CapabilityPolicy>,
) -> Result<(), String> {
    use crate::workspace_anchor::MountMode;
    let Some(anchor) = agent_sessions::workspace_anchor(id) else {
        return Ok(());
    };
    let Some(mounted) = anchor
        .additional_roots
        .iter()
        .find(|entry| entry.path == root)
    else {
        return Ok(());
    };
    let mut writable: Vec<std::path::PathBuf> = vec![anchor.primary.clone()];
    writable.extend(
        anchor
            .additional_roots
            .iter()
            .filter(|entry| matches!(entry.mount_mode, MountMode::Extend))
            .map(|entry| entry.path.clone()),
    );
    if let Some(policy) = policy
        .cloned()
        .or_else(crate::orchestration::current_execution_policy)
    {
        writable.extend(
            policy
                .workspace_roots
                .iter()
                .map(|root| crate::stdlib::sandbox::render_policy_root(root)),
        );
    }
    match mounted.mount_mode {
        MountMode::ReadOnly => {
            if let Some(inner) = writable
                .iter()
                .find(|other| *other != root && other.starts_with(root))
            {
                return Err(format!(
                    "cannot mount '{}' read_only: it contains the writable root '{}', which \
                     would stay writable inside it. Mount a narrower path.",
                    root.display(),
                    inner.display()
                ));
            }
        }
        MountMode::Extend => {
            if let Some(outer) = anchor.additional_roots.iter().find(|entry| {
                matches!(entry.mount_mode, MountMode::ReadOnly)
                    && entry.path != root
                    && root.starts_with(&entry.path)
            }) {
                return Err(format!(
                    "cannot mount '{}' writable: it is inside the read_only mount '{}'. \
                     Mount it elsewhere or remove the read_only mount first.",
                    root.display(),
                    outer.path.display()
                ));
            }
        }
        _ => {}
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use crate::workspace_anchor::{MountMode, WorkspaceAnchor};

    fn session(name: &str, primary: &std::path::Path) -> String {
        crate::reset_thread_local_state();
        let session = crate::agent_sessions::open_or_create_for_test(Some(name.to_string()));
        let anchor = WorkspaceAnchor {
            primary: primary.to_path_buf(),
            additional_roots: Vec::new(),
            anchored_at: "0".to_string(),
        };
        crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();
        session
    }

    fn mount(session: &str, root: &std::path::Path, mode: MountMode) -> Result<String, String> {
        super::add_workspace_root(session, &root.to_string_lossy(), Some(mode), None, None)
    }

    fn mounted(session: &str) -> usize {
        crate::agent_sessions::list_workspace_roots(session)
            .unwrap()
            .1
            .len()
    }

    #[test]
    fn a_read_only_mount_may_not_contain_a_writable_root() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("ws");
        let extra = dir.path().join("extra/inner");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let session = session("overlap-read-only-parent", &workspace);

        let parent = dir.path().canonicalize().unwrap();
        let refused = mount(&session, &parent, MountMode::ReadOnly).unwrap_err();
        assert!(refused.contains("contains the writable root"), "{refused}");
        assert_eq!(mounted(&session), 0, "the refused mount left nothing");

        mount(&session, &extra.canonicalize().unwrap(), MountMode::Extend).unwrap();
        let outer = extra.parent().unwrap().canonicalize().unwrap();
        let refused = mount(&session, &outer, MountMode::ReadOnly).unwrap_err();
        assert!(refused.contains("contains the writable root"), "{refused}");
        assert_eq!(mounted(&session), 1, "only the writable mount remains");
    }

    #[test]
    fn a_writable_mount_may_not_sit_inside_a_read_only_one() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("ws");
        let vendor = dir.path().join("vendor");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(vendor.join("lib")).unwrap();
        let session = session(
            "overlap-writable-inside",
            &workspace.canonicalize().unwrap(),
        );
        let vendor = vendor.canonicalize().unwrap();
        mount(&session, &vendor, MountMode::ReadOnly).unwrap();
        let refused = mount(&session, &vendor.join("lib"), MountMode::Extend).unwrap_err();
        assert!(refused.contains("inside the read_only mount"), "{refused}");
        assert_eq!(mounted(&session), 1);
        // A sibling with no containment either way is fine.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        mount(&session, &other.canonicalize().unwrap(), MountMode::Extend).unwrap();
        assert_eq!(mounted(&session), 2);
    }
}
