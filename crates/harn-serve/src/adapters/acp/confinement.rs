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

use std::path::{Path, PathBuf};

use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};
use harn_vm::process_sandbox::ProcessConfinement;
use harn_vm::{AutonomyTier, VmError};

use super::AcpServerConfig;

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
/// Call it after building `config` and before `run_acp_server`: the profile
/// comes from `config.sandbox`, plus read access to the served pipeline's
/// package and the package cache.
///
/// `Ok(None)` means the embedder's requested profile confines no process, so
/// the operator chose to run the agent with their own authority and the
/// server honours that. An error means the process is not confined and must
/// not serve.
pub fn confine_acp_server_process(
    config: &AcpServerConfig,
    confinement: &AcpServerConfinement,
) -> Result<Option<&'static ProcessConfinement>, VmError> {
    let Some(policy) = server_ceiling(config, confinement) else {
        return Ok(None);
    };
    // With no roots the profile would fall back to the current directory,
    // which is a confinement nobody chose.
    if confinement.workspace_roots.is_empty() {
        return Err(VmError::Runtime(
            "confining the server needs at least one workspace root".to_string(),
        ));
    }
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
    config: &AcpServerConfig,
    confinement: &AcpServerConfinement,
) -> Option<CapabilityPolicy> {
    let sandbox = &config.sandbox;
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
    for root in harn_read_roots(config.pipeline.as_deref()) {
        let root = root.display().to_string();
        if !policy.process_sandbox.read_roots.contains(&root) {
            policy.process_sandbox.read_roots.push(root);
        }
    }
    Some(policy)
}

/// What the server reads for itself, outside any workspace: the package the
/// served pipeline belongs to (its `harn.toml`, sibling modules, and assets)
/// and the installed package cache its imports resolve from. Read-only.
fn harn_read_roots(pipeline: Option<&str>) -> Vec<PathBuf> {
    harn_read_roots_under(pipeline, harn_vm::user_dirs::home_dir().as_deref())
}

fn harn_read_roots_under(pipeline: Option<&str>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = pipeline.and_then(|pipeline| Path::new(pipeline).parent()) {
        let package = harn_modules::manifest_walk::find_project_root(dir)
            .unwrap_or_else(|| dir.to_path_buf());
        // A package rooted at or above home would make every credential
        // directory readable; fall back to the pipeline's own directory.
        let contains_home = home.is_some_and(|home| home.starts_with(&package));
        roots.push(if contains_home {
            dir.to_path_buf()
        } else {
            package
        });
    }
    roots.extend(harn_vm::user_dirs::package_cache_dir());
    roots
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

    fn config() -> AcpServerConfig {
        AcpServerConfig::new(None)
    }

    fn confinement(root: &str) -> AcpServerConfinement {
        AcpServerConfinement {
            workspace_roots: vec![root.to_string()],
            state_roots: vec!["/state".to_string()],
        }
    }

    #[test]
    fn ceiling_confines_to_the_declared_workspace_and_state() {
        let policy =
            server_ceiling(&config(), &confinement("/work")).expect("the default profile confines");
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
        let mut config = config();
        config.sandbox.requested_profile = Some(SandboxProfile::OsHardened);
        let policy = server_ceiling(&config, &confinement("/work")).expect("os_hardened confines");
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
        let mut config = config();
        config.sandbox.requested_profile = Some(unconfined);
        assert!(server_ceiling(&config, &confinement("/work")).is_none());
    }

    #[test]
    fn confining_with_no_workspace_root_is_refused() {
        // Refused before anything is applied, so this test process stays
        // unconfined, which the next test relies on.
        let error = confine_acp_server_process(&config(), &AcpServerConfinement::default())
            .expect_err("no roots must not fall back to the current directory");
        assert!(error.to_string().contains("at least one workspace root"));
        assert!(harn_vm::process_sandbox::current_process_confinement().is_none());
    }

    #[test]
    fn ceiling_reads_the_served_package_but_never_from_home_up() {
        let package = tempfile::tempdir().unwrap();
        std::fs::write(package.path().join("harn.toml"), "").unwrap();
        std::fs::create_dir_all(package.path().join("agents")).unwrap();
        let pipeline = package.path().join("agents/main.harn");
        let pipeline = pipeline.to_str().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let roots = harn_read_roots_under(Some(pipeline), Some(elsewhere.path()));
        assert_eq!(roots[0], package.path());

        // Negative control: the same package treated as the home directory
        // contributes only the pipeline's own directory.
        let roots = harn_read_roots_under(Some(pipeline), Some(package.path()));
        assert_eq!(roots[0], package.path().join("agents"));
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
