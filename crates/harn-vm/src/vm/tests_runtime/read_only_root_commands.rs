//! A command under a `read` external root (harn#9622).
//!
//! The approval boundary admits a command-running call whose declared path is
//! under a `read` root only when the OS sandbox its child runs under refuses
//! every write there. Each test pairs that admission with the real spawn path,
//! so a disposition that claimed more than the kernel enforces would show up
//! as a write that landed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::harness::*;
use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, ExternalRoot, SandboxProfile,
    ToolApprovalPolicy,
};
use crate::stdlib::sandbox::{child_write_disposition, ChildWriteDisposition};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolArgSchema, ToolKind};

/// A confined-run policy: `workspace` writable, `read_only` readable, the
/// `run` tool annotated as executing.
fn confined_policy(
    profile: SandboxProfile,
    workspace: &Path,
    read_only: &[&Path],
) -> CapabilityPolicy {
    let mut annotations = BTreeMap::new();
    annotations.insert(
        "run".to_string(),
        ToolAnnotations {
            kind: ToolKind::Execute,
            side_effect_level: SideEffectLevel::ProcessExec,
            arg_schema: ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    CapabilityPolicy {
        // The child may write its workspace, as an agent's `run` may; without
        // that every path would be read-only and nothing here would be tested.
        capabilities: BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            (
                "workspace".to_string(),
                ["read_text", "write_text", "list", "exists"]
                    .map(str::to_string)
                    .to_vec(),
            ),
        ]),
        workspace_roots: vec![workspace.display().to_string()],
        read_only_roots: read_only
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        side_effect_level: Some("process_exec".to_string()),
        tool_annotations: annotations,
        sandbox_profile: profile,
        ..Default::default()
    }
}

/// The boundary's answer for `run` naming `path`, under `policy` and a `read`
/// external root at `root`.
fn run_admitted(policy: &CapabilityPolicy, root: &Path, path: &Path) -> bool {
    let approval = ToolApprovalPolicy {
        external_roots: vec![ExternalRoot::read(root.display().to_string())],
        ..Default::default()
    };
    push_execution_policy(policy.clone());
    let decision = approval.evaluate_detailed(
        "run",
        &serde_json::json!({ "path": path.display().to_string() }),
    );
    pop_execution_policy();
    decision.is_allow()
}

/// Run `script` through the VM's process capability under `policy`. Returns
/// whether the shell exited zero.
fn shell_succeeds(policy: &CapabilityPolicy, script: &str) -> bool {
    let source = format!(
        r"pipeline t(harness: Harness, task: unknown) {{ harness.process.shell({}) }}",
        serde_json::to_string(script).unwrap()
    );
    match run_harn_with_policy(&source, policy.clone()) {
        Ok((_, result)) => {
            result
                .as_dict()
                .and_then(|receipt| receipt.get("success").map(crate::VmValue::display))
                .as_deref()
                == Some("true")
        }
        // A refused spawn is a refused write.
        Err(_) => false,
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// Directories the default presets do not grant: the workspace under the
/// test's working directory and the external root under `HOME`, never under a
/// shared temp dir a preset might make writable.
fn workspace_and_outside() -> Option<(tempfile::TempDir, tempfile::TempDir)> {
    let cwd = std::env::current_dir().ok()?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if !home.is_dir() || home.starts_with("/tmp") || home.starts_with("/private/tmp") {
        return None;
    }
    Some((
        tempfile::tempdir_in(&cwd).ok()?,
        tempfile::tempdir_in(home).ok()?,
    ))
}

/// The disjoint case on a platform with a mechanism: the root is read-only
/// for the child, the boundary admits the command, and every write spelling
/// the issue names is refused by the kernel with nothing written.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_command_under_an_enforced_read_only_root_is_admitted_and_cannot_write() {
    if !crate::stdlib::sandbox::active_backend_filesystem_available() {
        return;
    }
    let Some((workspace, outside)) = workspace_and_outside() else {
        return;
    };
    let root = canonical(outside.path());
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("data.txt"), "reference").unwrap();
    let ws = canonical(workspace.path());
    std::os::unix::fs::symlink(&root, ws.join("link")).unwrap();
    let policy = confined_policy(SandboxProfile::OsHardened, &ws, &[root.as_path()]);

    let file = root.join("data.txt");
    assert_eq!(
        child_write_disposition(&policy, &file),
        ChildWriteDisposition::EnforcedReadOnly
    );
    assert!(
        run_admitted(&policy, &root, &file),
        "the boundary must admit the read"
    );
    assert!(
        shell_succeeds(&policy, &format!("cat '{}'", file.display())),
        "control: the child can read the root"
    );

    for (label, target) in [
        ("direct", root.join("f")),
        ("dot-dot", root.join("sub").join("..").join("f")),
        ("workspace symlink", ws.join("link").join("f")),
    ] {
        assert!(
            !shell_succeeds(&policy, &format!("echo x > '{}'", target.display())),
            "{label}: the kernel must refuse a write into the read-only root"
        );
        assert!(!root.join("f").exists(), "{label}: nothing may be written");
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "reference");
    // Control: the same policy writes the workspace, so the refusals above are
    // the root's mode and not a child that cannot write anything.
    assert!(shell_succeeds(
        &policy,
        &format!("echo x > '{}'", ws.join("ok.txt").display())
    ));
}

/// A read-only root nested in the writable workspace: the oracle must agree
/// with the kernel on both OSes. macOS re-denies it, so it is read-only;
/// Landlock cannot, so it is `Writable`, and the write really lands. (The
/// boundary never consults the oracle here: a workspace path is not under an
/// external root.)
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_read_only_root_nested_in_the_workspace_is_judged_by_its_backend() {
    if !crate::stdlib::sandbox::active_backend_filesystem_available() {
        return;
    }
    let Some((workspace, _outside)) = workspace_and_outside() else {
        return;
    };
    let ws = canonical(workspace.path());
    let nested = ws.join("vendor");
    std::fs::create_dir_all(&nested).unwrap();
    let policy = confined_policy(SandboxProfile::OsHardened, &ws, &[nested.as_path()]);
    let target = nested.join("f");
    let write = format!("echo x > '{}'", target.display());

    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            child_write_disposition(&policy, &target),
            ChildWriteDisposition::EnforcedReadOnly
        );
        assert!(!shell_succeeds(&policy, &write));
        assert!(!target.exists());
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            child_write_disposition(&policy, &target),
            ChildWriteDisposition::Writable
        );
        assert!(
            shell_succeeds(&policy, &write) && target.exists(),
            "the oracle said writable, so the write must really land"
        );
    }
}

/// Without `os_hardened` nothing is measured: `worktree` may run unconfined
/// on a host without the mechanism, and `unrestricted` never confines.
#[test]
fn only_os_hardened_can_admit() {
    let Some((workspace, outside)) = workspace_and_outside() else {
        return;
    };
    let root = canonical(outside.path());
    let file = root.join("data.txt");
    std::fs::write(&file, "reference").unwrap();
    let ws = canonical(workspace.path());
    for profile in [SandboxProfile::Unrestricted, SandboxProfile::Worktree] {
        let policy = confined_policy(profile, &ws, &[root.as_path()]);
        assert_eq!(
            child_write_disposition(&policy, &file),
            ChildWriteDisposition::Unknown,
            "{profile:?}"
        );
        assert!(!run_admitted(&policy, &root, &file), "{profile:?}");
    }
}

/// No active policy means no measured fact: the boundary keeps refusing.
#[test]
fn a_command_with_no_active_policy_is_refused() {
    let Some((_workspace, outside)) = workspace_and_outside() else {
        return;
    };
    let root = canonical(outside.path());
    let file = root.join("data.txt");
    let approval = ToolApprovalPolicy {
        external_roots: vec![ExternalRoot::read(root.display().to_string())],
        ..Default::default()
    };
    let decision = approval.evaluate_detailed(
        "run",
        &serde_json::json!({ "path": file.display().to_string() }),
    );
    assert!(!decision.is_allow(), "{}", decision.reason);
}

/// Windows has no OS mechanism, so even `os_hardened` measures nothing.
#[cfg(windows)]
#[test]
fn windows_never_admits() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = canonical(outside.path());
    let file = root.join("data.txt");
    std::fs::write(&file, "reference").unwrap();
    let policy = confined_policy(
        SandboxProfile::OsHardened,
        &canonical(workspace.path()),
        &[root.as_path()],
    );
    assert_eq!(
        child_write_disposition(&policy, &file),
        ChildWriteDisposition::Unknown
    );
    assert!(!run_admitted(&policy, &root, &file));
}
