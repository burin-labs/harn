//! One normalized, opened grant set for both Linux filesystem renderers.

use std::path::{Path, PathBuf};

use super::{
    developer_toolchain_system_read_roots, network_name_service_read_roots,
    proc_runtime_reads_are_contained, push_rule, read_only_access, standard_device_rules,
    system_read_roots, workspace_access, AccessFs, FilesystemProfile, FsAccess,
    ProcessFilesystemScope,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::stdlib::sandbox::{
    policy_allows_child_writes, process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_readonly_roots, process_sandbox_roots,
    sandbox_rejection, warn_once,
};
use crate::VmError;

/// Credential subtraction and optional-root handling cannot drift by backend.
pub(super) fn filesystem_profile(
    program: &str,
    policy: &CapabilityPolicy,
    handled_access_fs: FsAccess,
    process_scope: ProcessFilesystemScope,
) -> Result<FilesystemProfile, VmError> {
    let mut profile = FilesystemProfile {
        rules: Vec::new(),
        symlinks: std::collections::BTreeMap::new(),
        handled_access_fs,
        read_deny_roots: super::super::process_sandbox_read_deny_roots(policy),
        unheld_read_denials: Vec::new(),
    };
    for (path, access) in standard_device_rules() {
        push_rule(&mut profile, path, access, true)?;
    }
    for path in system_read_roots() {
        push_rule(&mut profile, path, read_only_access(), true)?;
    }
    for path in network_name_service_read_roots(policy) {
        // `/etc/resolv.conf` is commonly a symlink into `/run` on hosted
        // Linux. Landlock checks the resolved inode, so the broad `/etc` rule
        // above does not cover that target. Open each exact host file before
        // confinement and grant its canonical inode without exposing `/run`.
        push_rule(&mut profile, path, AccessFs::ReadFile.into(), true)?;
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
            AccessFs::ReadFile | AccessFs::ReadDir,
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
            AccessFs::ReadFile.into(),
            true,
        )?;
    }
    // An absolute executable grants only that file, never its parent.
    let program_path = std::path::Path::new(program);
    if program_path.is_absolute() {
        push_rule(
            &mut profile,
            program_path.to_path_buf(),
            AccessFs::ReadFile | AccessFs::Execute,
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
                AccessFs::MakeSock
                    | AccessFs::ReadFile
                    | AccessFs::ReadDir
                    | AccessFs::WriteFile
                    | AccessFs::MakeDir
                    | AccessFs::RemoveFile,
                true,
            )?;
        }
    }
    report_unheld_read_denials(policy, &profile.unheld_read_denials)?;
    Ok(profile)
}

/// A read denial inside a writable grant, which that grant cannot hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UnheldReadDenial {
    pub(super) denied: PathBuf,
    pub(super) writable_grant: PathBuf,
}

/// Whether to issue the writable `grant`, recording each denial inside it.
///
/// A read-only grant is subtracted around a denial (see
/// [`super::expand_around_denied`]). A writable one is not. Landlock could hold
/// the denial only by withholding every directory on the path to it, and a
/// child refused those directories cannot create a file anywhere along that
/// path (harn#9028). The grant keeps its whole root, so the denial is not
/// enforced inside it, and that is reported rather than silent. A grant that
/// sits inside a denial is still withheld.
pub(super) fn keep_writable_grant(profile: &mut FilesystemProfile, grant: &Path) -> bool {
    if profile
        .read_deny_roots
        .iter()
        .any(|denied| grant.starts_with(denied))
    {
        return false;
    }
    for denied in &profile.read_deny_roots {
        let unheld = UnheldReadDenial {
            denied: denied.clone(),
            writable_grant: grant.to_path_buf(),
        };
        if denied.starts_with(grant) && !profile.unheld_read_denials.contains(&unheld) {
            profile.unheld_read_denials.push(unheld);
        }
    }
    true
}

/// An unheld denial refuses an `os_hardened` spawn, as every other requirement
/// a backend cannot hold does. Other profiles run, and the warning names each
/// denial and the writable root that contains it.
fn report_unheld_read_denials(
    policy: &CapabilityPolicy,
    unheld: &[UnheldReadDenial],
) -> Result<(), VmError> {
    if unheld.is_empty() {
        return Ok(());
    }
    let pairs = unheld
        .iter()
        .map(|entry| {
            format!(
                "'{}' inside writable root '{}'",
                entry.denied.display(),
                entry.writable_grant.display()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let fact = format!(
        "credential read denials are not enforced for {pairs}: the writable root is granted \
         whole, so a child can read the denied path"
    );
    if policy.sandbox_profile == SandboxProfile::OsHardened {
        return Err(sandbox_rejection(format!(
            "{fact}; the requested sandbox profile requires every denial to hold. Move the \
             denied path out of the writable root, or grant that root read-only."
        )));
    }
    warn_once(
        &format!("sandbox_unheld_read_denial:{pairs}"),
        &format!("{fact}. The writable roots keep their full access."),
    );
    Ok(())
}

/// The write side of the grant set a confined child would run under, for
/// [`super::super::child_write::child_write_disposition`]. Built by the same
/// `filesystem_profile` both Linux renderers install, so it cannot claim a
/// rule the kernel does not get. `None` when Landlock is unavailable here.
pub(crate) fn installed_write_grants(
    policy: &CapabilityPolicy,
) -> Option<super::super::child_write::LandlockWriteGrants> {
    let support = super::landlock_support::host_support()?;
    let profile = filesystem_profile(
        "/bin/sh",
        policy,
        support.handled_access(),
        ProcessFilesystemScope::Host,
    )
    .ok()?;
    // Two write rights an older ABI leaves unhandled are still refused
    // outside the grants: below ABI 2 the kernel refuses every reparenting
    // rename or link, and below ABI 3 the seccomp filter refuses truncation
    // by path.
    let refused_outside_grants = support.handled_access() | AccessFs::Refer | AccessFs::Truncate;
    Some(super::super::child_write::LandlockWriteGrants {
        rules: profile
            .filesystem_rules()
            .map(|(path, access)| (path.to_path_buf(), access.bits()))
            .collect(),
        handled_access: refused_outside_grants.bits(),
        write_access: LANDLOCK_WRITE_ACCESS.bits(),
    })
}

impl FilesystemProfile {
    fn filesystem_rules(&self) -> impl Iterator<Item = (&std::path::Path, FsAccess)> + '_ {
        self.rules
            .iter()
            .map(|rule| (rule.path.as_path(), rule.allowed_access))
    }
}

/// Every Landlock right that changes the filesystem, through truncation.
/// `IOCTL_DEV` is left out: no write grant needs to drive a device.
pub(super) const LANDLOCK_WRITE_ACCESS: FsAccess = super::landlock_support::make_bitflags!(AccessFs::{
    WriteFile
        | RemoveDir
        | RemoveFile
        | MakeChar
        | MakeDir
        | MakeReg
        | MakeSock
        | MakeFifo
        | MakeBlock
        | MakeSym
        | Refer
        | Truncate
});
