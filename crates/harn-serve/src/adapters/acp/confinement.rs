//! Confining an out-of-process ACP server to its workspace.
//!
//! An agent server interprets model output and runs pipeline code, so a flaw
//! in either is a flaw with the server's authority. A stdio server that is its
//! own process can be held by the kernel to the same boundary its confined
//! commands get, so that a compromised VM cannot reach what a confined command
//! cannot. The ceiling is computed here, from the embedder's sandbox config,
//! so every embedder confines its server the same way.
//!
//! Only a dedicated server process may do this. Confinement is process-wide
//! and permanent, so an in-process (channel) server would confine its host.

use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};
use harn_vm::process_sandbox::ProcessConfinement;
use harn_vm::{AutonomyTier, VmError};

use super::AcpSandboxConfig;

/// What an embedder declares about the server process it is confining.
#[derive(Clone, Debug, Default)]
pub struct AcpServerConfinement {
    /// The directories the agent works in, writable as they are for a
    /// confined command.
    pub workspace_roots: Vec<String>,
    /// The embedder's own state directories outside the workspace that the
    /// server writes (session stores, logs). Writable to the server and so,
    /// on macOS, to the commands it runs, which inherit its profile.
    pub state_roots: Vec<String>,
}

/// Confine this process, for the rest of its life, before it serves ACP.
///
/// `Ok(None)` means the embedder's requested profile confines no process, so
/// the operator chose to run the agent with their own authority and the
/// server honours that. An error means the process is not confined and must
/// not serve.
pub fn confine_acp_server_process(
    sandbox: &AcpSandboxConfig,
    confinement: &AcpServerConfinement,
) -> Result<Option<&'static ProcessConfinement>, VmError> {
    let Some(policy) = server_ceiling(sandbox, confinement) else {
        return Ok(None);
    };
    harn_vm::process_sandbox::confine_current_process(&policy).map(Some)
}

/// The widest policy any session on this server may run under, as one
/// process profile, or `None` when the requested profile confines nothing.
///
/// Modes change per turn but the process profile cannot, so the ceiling is
/// the act-auto tier's: the server itself makes model calls whatever mode a
/// session is in, and a session that may switch to `code` must be able to
/// write its workspace. Per-turn mode policy still narrows Harn's own tools.
fn server_ceiling(
    sandbox: &AcpSandboxConfig,
    confinement: &AcpServerConfinement,
) -> Option<CapabilityPolicy> {
    let mut policy = harn_vm::policy_for_autonomy_tier(AutonomyTier::ActAuto);
    policy.sandbox_profile = sandbox
        .requested_profile
        .unwrap_or(SandboxProfile::Worktree);
    if !policy.sandbox_profile.confines_processes() {
        return None;
    }
    policy.workspace_roots = confinement.workspace_roots.clone();
    super::modes::apply_sandbox_config(&mut policy, sandbox);
    policy.process_sandbox.allow_child_workspace_write = true;
    for root in &confinement.state_roots {
        if !policy.process_sandbox.write_roots.contains(root) {
            policy.process_sandbox.write_roots.push(root.clone());
        }
    }
    Some(policy)
}

/// This process's confinement, as `initialize` reports it in
/// `agentCapabilities._meta.harn.processConfinement`.
///
/// Always present, so a host can tell an unconfined server from one too old
/// to say.
pub(super) fn initialize_meta() -> serde_json::Value {
    match harn_vm::process_sandbox::current_process_confinement() {
        Some(confinement) => serde_json::json!({
            "state": "confined",
            "backend": confinement.backend,
            "mechanism": confinement.mechanism,
            "workspaceRoots": confinement.workspace_roots,
            "tempDir": confinement.temp_dir,
        }),
        None => serde_json::json!({ "state": "unconfined" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confinement(root: &str) -> AcpServerConfinement {
        AcpServerConfinement {
            workspace_roots: vec![root.to_string()],
            state_roots: vec!["/state".to_string()],
        }
    }

    #[test]
    fn ceiling_confines_to_the_declared_workspace_and_state() {
        let policy = server_ceiling(&AcpSandboxConfig::default(), &confinement("/work"))
            .expect("the default profile confines");
        assert_eq!(policy.sandbox_profile, SandboxProfile::Worktree);
        assert_eq!(policy.workspace_roots, vec!["/work".to_string()]);
        assert!(policy.process_sandbox.allow_child_workspace_write);
        assert_eq!(
            policy.process_sandbox.write_roots,
            vec!["/state".to_string()]
        );
    }

    #[test]
    fn ceiling_honours_an_explicit_profile() {
        let sandbox = AcpSandboxConfig {
            requested_profile: Some(SandboxProfile::OsHardened),
            ..AcpSandboxConfig::default()
        };
        let policy = server_ceiling(&sandbox, &confinement("/work")).expect("os_hardened confines");
        assert_eq!(policy.sandbox_profile, SandboxProfile::OsHardened);
    }

    #[test]
    fn an_unconfined_profile_leaves_the_server_unconfined() {
        // Negative control for the test above: the same call under a profile
        // that confines no process must not produce a ceiling to apply.
        let unconfined = SandboxProfile::all()
            .iter()
            .copied()
            .find(|profile| !profile.confines_processes())
            .expect("some profile confines no process");
        let sandbox = AcpSandboxConfig {
            requested_profile: Some(unconfined),
            ..AcpSandboxConfig::default()
        };
        assert!(server_ceiling(&sandbox, &confinement("/work")).is_none());
    }

    #[test]
    fn an_unconfined_process_says_so_in_initialize() {
        // Test processes are never confined: confinement is permanent.
        assert_eq!(
            initialize_meta(),
            serde_json::json!({ "state": "unconfined" })
        );
    }
}
