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
    // The host temp dir is shared with every other process the user runs, so
    // it is not granted. The process gets a private one instead: Harn writes
    // command artifacts and scratch files through `TMPDIR`.
    let temp_dir = private_temp_dir()?;
    let mut policy = policy.clone();
    policy
        .process_sandbox
        .write_roots
        .push(temp_dir.display().to_string());
    let mechanism = ActiveBackend::confine_current_process(&policy)?;
    // SAFETY: called once, before the process serves; `temp_dir()` and child
    // environments read `TMPDIR` afterwards, and nothing reads it concurrently.
    unsafe { std::env::set_var("TMPDIR", &temp_dir) };
    let confinement = ProcessConfinement {
        backend: ActiveBackend::name(),
        mechanism,
        workspace_roots: sandbox::process_sandbox_roots(&policy),
        temp_dir,
    };
    let confinement = CONFINEMENT.get_or_init(|| confinement);
    report(confinement);
    Ok(confinement)
}

/// A fresh directory under the host temp dir that only this process uses.
fn private_temp_dir() -> Result<PathBuf, VmError> {
    let dir = sandbox::normalize_for_policy(
        &std::env::temp_dir().join(format!("harn-confined-{}", std::process::id())),
    );
    let created = std::fs::create_dir_all(&dir).and_then(|()| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    });
    created.map_err(|error| {
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
