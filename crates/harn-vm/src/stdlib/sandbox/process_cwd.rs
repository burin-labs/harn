//! Where a subprocess may be launched from.
//!
//! Kept apart from the filesystem-scope checks in [`super`] because it answers
//! a different question: those decide what Harn's own file builtins may touch,
//! this decides where a child process may start. A policy can grant the second
//! without the first.

use std::path::{Path, PathBuf};

use super::{
    normalize_for_policy, normalized_process_roots, normalized_workspace_roots, path_is_within,
    sandbox_rejection,
};
use crate::orchestration::CapabilityPolicy;
use crate::value::VmError;

/// The directories a subprocess may be launched from.
///
/// Where a child starts is a process-axis question, so this accepts the
/// process-only roots alongside the workspace roots. `ProcessSandboxPolicy`
/// exists to hand a child a directory *without* also handing Harn's file
/// builtins read or write access to it; a policy that could grant a subprocess
/// a root but could not start the subprocess there would make that grant
/// unusable for its stated purpose. Reading a directory is what launching from
/// it needs, so a read root is enough.
fn process_cwd_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = normalized_workspace_roots(policy);
    for root in normalized_process_roots(&policy.process_sandbox.read_roots)
        .into_iter()
        .chain(normalized_process_roots(
            &policy.process_sandbox.write_roots,
        ))
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

pub(super) fn enforce_process_cwd_for_policy(
    path: &Path,
    policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    if !policy.sandbox_profile.enforces_path_scope() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = process_cwd_roots(policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: process cwd '{}' is outside the launchable roots [{}]",
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}
