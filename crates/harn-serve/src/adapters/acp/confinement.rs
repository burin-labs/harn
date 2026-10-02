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
    let own = own_roots(config.pipeline.as_deref());
    for (roots, granted) in [
        (own.read, &mut policy.process_sandbox.read_roots),
        (own.write, &mut policy.process_sandbox.write_roots),
    ] {
        for root in roots {
            let root = root.display().to_string();
            if !granted.contains(&root) {
                granted.push(root);
            }
        }
    }
    Some(policy)
}

/// What the server needs for itself outside any workspace.
#[derive(Debug, Default, PartialEq)]
struct OwnRoots {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

/// The package the served pipeline belongs to, readable (its `harn.toml`,
/// sibling modules, and installed package generations), with its `.harn`
/// package state writable for Harn's install and snapshot locks; and the
/// installed package cache, readable.
fn own_roots(pipeline: Option<&str>) -> OwnRoots {
    own_roots_under(pipeline, harn_vm::user_dirs::home_dir().as_deref())
}

fn own_roots_under(pipeline: Option<&str>, home: Option<&Path>) -> OwnRoots {
    let mut roots = OwnRoots::default();
    if let Some(dir) = pipeline.and_then(|pipeline| Path::new(pipeline).parent()) {
        // Harn finds a package by its manifest or by its installed package
        // state, whichever is nearer; read the same root it will.
        let package = [
            harn_modules::manifest_walk::find_project_root(dir),
            harn_modules::package_snapshot::PackageSnapshot::nearest_project_root(dir),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|root| root.components().count());
        // A package rooted at or above home would make every credential
        // directory readable; fall back to the pipeline's own directory.
        match package.filter(|package| !home.is_some_and(|home| home.starts_with(package))) {
            Some(package) => {
                roots
                    .write
                    .push(harn_modules::package_snapshot::package_state_dir(&package));
                roots.read.push(package);
            }
            None => roots.read.push(dir.to_path_buf()),
        }
    }
    roots.read.extend(harn_vm::user_dirs::package_cache_dir());
    // Landlock grants on an open handle, so a root that doesn't exist can't be
    // granted at all; there is also nothing in it to read or lock.
    roots.read.retain(|root| root.exists());
    roots.write.retain(|root| root.exists());
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
        let roots = own_roots_under(Some(pipeline), Some(elsewhere.path()));
        assert_eq!(roots.read[0], package.path());
        // No `.harn` state yet, so nothing to grant write on.
        assert!(roots.write.is_empty());
        std::fs::create_dir(package.path().join(".harn")).unwrap();
        let roots = own_roots_under(Some(pipeline), Some(elsewhere.path()));
        assert_eq!(roots.write, vec![package.path().join(".harn")]);

        // Negative control: the same package treated as the home directory
        // contributes only the pipeline's own directory, and nothing writable.
        let roots = own_roots_under(Some(pipeline), Some(package.path()));
        assert_eq!(roots.read[0], package.path().join("agents"));
        assert!(roots.write.is_empty());
    }

    #[test]
    fn ceiling_finds_a_package_by_its_installed_state_alone() {
        // A staged package can carry installed generations and no manifest.
        let package = tempfile::tempdir().unwrap();
        let state = package.path().join(".harn");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("package-current.toml"), "").unwrap();
        std::fs::create_dir_all(package.path().join("pipelines/mode")).unwrap();
        let pipeline = package.path().join("pipelines/mode/auto.harn");
        let elsewhere = tempfile::tempdir().unwrap();
        let roots = own_roots_under(pipeline.to_str(), Some(elsewhere.path()));
        assert_eq!(roots.read[0], package.path());
        assert_eq!(roots.write, vec![state]);
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
