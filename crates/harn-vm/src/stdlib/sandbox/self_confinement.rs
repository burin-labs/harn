//! Confining the running process itself under a capability policy.
//!
//! Every other entry point in this module confines a child at spawn. This one
//! confines the caller: a long-lived agent server that interprets model output
//! and runs untrusted pipeline code calls it once, before it serves, so a
//! compromise of the VM is held by the kernel to the same workspace boundary a
//! confined command gets, instead of inheriting the user's full authority.
//!
//! The profile is the one the backend renders for a child under the same
//! policy. There is no second renderer to drift: a root granted here is a root
//! a confined command would be granted, and a credential directory denied to
//! one is denied to the other.
//!
//! Confinement cannot be undone or widened, and on macOS it cannot be stacked:
//! once this process is confined, children it spawns run under its profile
//! rather than a narrower one of their own (see `macos/nested.rs`, which
//! refuses a child whose policy the outer profile would silently widen). On
//! Linux, Landlock domains stack, so a child's own ruleset still narrows it
//! further.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::orchestration::CapabilityPolicy;
use crate::value::VmError;

use super::{ActiveBackend, SandboxBackend, SandboxMechanism};
use crate::stdlib::sandbox;

/// A record of this process's own confinement, set once and never cleared.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProcessConfinement {
    /// The platform backend that applied it, as `active_backend_name` reports.
    pub backend: &'static str,
    /// The kernel mechanism now enforcing it.
    pub mechanism: SandboxMechanism,
    /// The workspace roots the process may write, as the profile rendered them.
    pub workspace_roots: Vec<PathBuf>,
    /// The process's private temp dir, which `TMPDIR` now names.
    pub temp_dir: PathBuf,
}

impl ProcessConfinement {
    /// Whether `path` lies inside a root this process may write.
    pub fn covers(&self, path: &std::path::Path) -> bool {
        let path = sandbox::normalize_for_policy(path);
        self.workspace_roots
            .iter()
            .any(|root| path.starts_with(root))
    }
}

static CONFINEMENT: OnceLock<ProcessConfinement> = OnceLock::new();

/// Confine the calling process under `policy`, for the rest of its life.
///
/// Fails closed: an error means the process is NOT confined and the caller
/// must not go on to serve as if it were. Refuses a policy whose sandbox
/// profile confines no process, since there is then nothing to apply, and a
/// second call, since a confinement can be neither replaced nor stacked.
pub fn confine_current_process(
    policy: &CapabilityPolicy,
) -> Result<&'static ProcessConfinement, VmError> {
    confine_with::<ActiveBackend>(policy)
}

fn confine_with<B: SandboxBackend>(
    policy: &CapabilityPolicy,
) -> Result<&'static ProcessConfinement, VmError> {
    if CONFINEMENT.get().is_some() {
        return Err(sandbox::sandbox_rejection(
            "this process is already confined, and a confinement cannot be replaced".to_string(),
        ));
    }
    if !policy.sandbox_profile.confines_processes() {
        return Err(sandbox::sandbox_rejection(format!(
            "the `{}` sandbox profile confines no process, so there is nothing to confine this \
             process with",
            policy.sandbox_profile.as_str()
        )));
    }
    // A root that contains the home directory makes the profile a formality:
    // it hands the process the user's dotfiles, shell startup files, and keys.
    let home = crate::user_dirs::home_dir().map(|home| sandbox::normalize_for_policy(&home));
    if let Some(root) = sandbox::process_sandbox_roots(policy)
        .into_iter()
        .find(|root| {
            root.parent().is_none() || home.as_ref().is_some_and(|home| home.starts_with(root))
        })
    {
        return Err(sandbox::sandbox_rejection(format!(
            "`{}` contains the home directory, so confining to it would confine nothing",
            root.display()
        )));
    }
    // The host temp dir is shared with every other process the user runs, so
    // it is not granted. The process gets a private one instead: Harn writes
    // command artifacts and scratch files through `TMPDIR`.
    let temp_dir = private_temp_dir()?;
    let mut policy = policy.clone();
    policy
        .process_sandbox
        .write_roots
        .push(temp_dir.display().to_string());
    let mechanism = match B::confine_current_process(&policy) {
        Ok(mechanism) => mechanism,
        Err(error) => {
            // Nothing was applied, so nothing will use the directory.
            let _ = std::fs::remove_dir(&temp_dir);
            return Err(error);
        }
    };
    // SAFETY: called once, before the process serves its first message.
    // Runtime threads may already exist, but none is reading the environment
    // yet: they are parked until the server starts handing them work.
    unsafe { std::env::set_var("TMPDIR", &temp_dir) };
    let confinement = ProcessConfinement {
        backend: B::name(),
        mechanism,
        workspace_roots: sandbox::process_sandbox_roots(&policy),
        temp_dir,
    };
    let confinement = CONFINEMENT.get_or_init(|| confinement);
    report(confinement);
    Ok(confinement)
}

/// A fresh directory under the host temp dir that only this process uses.
///
/// Created with a random name and without following an existing entry, so a
/// name another user planted in a shared temp dir (a symlink to somewhere
/// they want written) is an error rather than a grant.
fn private_temp_dir() -> Result<PathBuf, VmError> {
    let parent = sandbox::normalize_for_policy(&std::env::temp_dir());
    let dir = parent.join(format!("harn-confined-{}", uuid::Uuid::new_v4().simple()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).map_err(|error| {
        sandbox::sandbox_rejection(format!(
            "cannot create the private temp dir {}: {error}",
            dir.display()
        ))
    })?;
    Ok(dir)
}

/// This process's own confinement, if [`confine_current_process`] applied one.
pub fn current_process_confinement() -> Option<&'static ProcessConfinement> {
    CONFINEMENT.get()
}

fn report(confinement: &ProcessConfinement) {
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert(
        "backend".to_string(),
        serde_json::json!(confinement.backend),
    );
    metadata.insert(
        "mechanism".to_string(),
        serde_json::json!(confinement.mechanism.as_str()),
    );
    metadata.insert(
        "workspace_roots".to_string(),
        serde_json::json!(confinement.workspace_roots),
    );
    metadata.insert(
        "temp_dir".to_string(),
        serde_json::json!(confinement.temp_dir),
    );
    crate::events::log_info_meta(
        "process_sandbox_self",
        "this process is now confined by the kernel to its workspace profile",
        metadata,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::SandboxProfile;

    fn policy_for(root: &std::path::Path) -> CapabilityPolicy {
        CapabilityPolicy {
            workspace_roots: vec![root.display().to_string()],
            sandbox_profile: SandboxProfile::Worktree,
            ..Default::default()
        }
    }

    // Each refusal happens before anything is applied, so the test process
    // stays unconfined.
    #[test]
    fn a_root_containing_home_is_refused() {
        let home = crate::user_dirs::home_dir().expect("a home directory");
        for root in [home.clone(), home.parent().unwrap().to_path_buf()] {
            let error = confine_current_process(&policy_for(&root)).unwrap_err();
            assert!(
                error.to_string().contains("contains the home directory"),
                "{error}"
            );
        }
        assert!(current_process_confinement().is_none());
    }

    /// A backend that cannot confine must leave the process unconfined and
    /// say so, not report success: the caller would otherwise serve open.
    #[test]
    fn a_backend_failure_fails_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let error = confine_with::<super::super::UnconfinedBackend>(&policy_for(workspace.path()))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot confine a running process"),
            "{error}"
        );
        assert!(current_process_confinement().is_none());
        assert!(std::env::var_os("TMPDIR")
            .is_none_or(|tmp| { !tmp.to_string_lossy().contains("harn-confined-") }));
    }

    #[test]
    fn a_profile_that_confines_nothing_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Unrestricted,
            ..policy_for(workspace.path())
        };
        let error = confine_current_process(&policy).unwrap_err();
        assert!(error.to_string().contains("confines no process"), "{error}");
        assert!(current_process_confinement().is_none());
    }
}
