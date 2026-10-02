//! One normalized, opened grant set for both Linux filesystem renderers.

use std::path::PathBuf;

use super::{
    developer_toolchain_system_read_roots, network_name_service_read_roots,
    proc_runtime_reads_are_contained, push_rule, read_only_access, standard_device_rules,
    system_read_roots, workspace_access, FilesystemProfile, ProcessFilesystemScope,
    LANDLOCK_ACCESS_FS_EXECUTE, LANDLOCK_ACCESS_FS_MAKE_DIR, LANDLOCK_ACCESS_FS_MAKE_SOCK,
    LANDLOCK_ACCESS_FS_READ_DIR, LANDLOCK_ACCESS_FS_READ_FILE, LANDLOCK_ACCESS_FS_REMOVE_FILE,
    LANDLOCK_ACCESS_FS_WRITE_FILE,
};
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::{
    policy_allows_child_writes, process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_readonly_roots, process_sandbox_roots,
    sandbox_rejection,
};
use crate::VmError;

/// Credential subtraction and optional-root handling cannot drift by backend.
pub(super) fn filesystem_profile(
    program: &str,
    policy: &CapabilityPolicy,
    handled_access_fs: u64,
    process_scope: ProcessFilesystemScope,
) -> Result<FilesystemProfile, VmError> {
    let mut profile = FilesystemProfile {
        rules: Vec::new(),
        symlinks: std::collections::BTreeMap::new(),
        handled_access_fs,
        read_deny_roots: super::super::process_sandbox_read_deny_roots(policy),
    };
    for (path, access) in standard_device_rules() {
        push_rule(&mut profile, path, access, true)?;
    }
    for path in system_read_roots() {
        push_rule(
            &mut profile,
            path,
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR | LANDLOCK_ACCESS_FS_EXECUTE,
            true,
        )?;
    }
    for path in network_name_service_read_roots(policy) {
        // `/etc/resolv.conf` is commonly a symlink into `/run` on hosted
        // Linux. Landlock checks the resolved inode, so the broad `/etc` rule
        // above does not cover that target. Open each exact host file before
        // confinement and grant its canonical inode without exposing `/run`.
        push_rule(&mut profile, path, LANDLOCK_ACCESS_FS_READ_FILE, true)?;
    }
    if policy.process_sandbox.allow_process_self_introspection {
        // A host procfs grant needs kernel containment; a private PID
        // namespace already limits the visible process tree.
        if matches!(process_scope, ProcessFilesystemScope::Host)
            && !proc_runtime_reads_are_contained()
        {
            return Err(sandbox_rejection(
                "process self-introspection needs a kernel that keeps a sandboxed task from inspecting its neighbours; this host permits it, so the grant would widen past the process it names"
                    .to_string(),
            ));
        }
        // Landlock resolves `/proc/self` to this process's inode. Descendants
        // need their own process directories, so the containing `/proc` rule
        // is only issued with the containment established above.
        push_rule(
            &mut profile,
            PathBuf::from("/proc"),
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR,
            true,
        )?;
    } else if proc_runtime_reads_are_contained()
        || matches!(process_scope, ProcessFilesystemScope::PrivatePidNamespace)
    {
        // Runtime memory-map reads may cover descendants, but directory
        // enumeration remains outside this file-only grant.
        push_rule(
            &mut profile,
            PathBuf::from("/proc"),
            LANDLOCK_ACCESS_FS_READ_FILE,
            true,
        )?;
    }
    // An absolute executable grants only that file, never its parent.
    let program_path = std::path::Path::new(program);
    if program_path.is_absolute() {
        push_rule(
            &mut profile,
            program_path.to_path_buf(),
            LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_EXECUTE,
            true,
        )?;
    }
    for root in process_sandbox_developer_toolchain_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    for root in developer_toolchain_system_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    for grant in super::super::read_roots::path_grants::process_sandbox_path_entry_grants(policy) {
        push_rule(&mut profile, grant.root, read_only_access(), true)?;
    }
    let workspace_access = workspace_access(policy);
    for root in process_sandbox_roots(policy) {
        push_rule(&mut profile, root, workspace_access, false)?;
    }
    for root in process_sandbox_readonly_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), false)?;
    }
    for root in process_sandbox_policy_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), false)?;
    }
    for root in process_sandbox_package_manager_config_read_roots(policy) {
        push_rule(&mut profile, root, read_only_access(), true)?;
    }
    // Optional caches may be populated only when child writes are granted.
    let toolchain_cache_roots =
        super::super::process_sandbox_developer_toolchain_cache_roots(policy);
    let toolchain_cache_access = if policy_allows_child_writes(policy) {
        workspace_access
    } else {
        read_only_access()
    };
    for root in toolchain_cache_roots {
        push_rule(&mut profile, root, toolchain_cache_access, true)?;
    }
    if policy_allows_child_writes(policy) {
        for root in process_sandbox_policy_write_roots(policy) {
            push_rule(&mut profile, root, workspace_access, false)?;
        }
    }
    // Socket creation authority comes from seccomp; these roots own where a
    // socket file may be bound, including policies that also allow networking.
    if !policy.process_sandbox.unix_socket_roots.is_empty() {
        for root in super::super::process_sandbox_unix_socket_roots(policy) {
            push_rule(
                &mut profile,
                root,
                LANDLOCK_ACCESS_FS_MAKE_SOCK
                    | LANDLOCK_ACCESS_FS_READ_FILE
                    | LANDLOCK_ACCESS_FS_READ_DIR
                    | LANDLOCK_ACCESS_FS_WRITE_FILE
                    | LANDLOCK_ACCESS_FS_MAKE_DIR
                    | LANDLOCK_ACCESS_FS_REMOVE_FILE,
                true,
            )?;
        }
    }
    Ok(profile)
}
