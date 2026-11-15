//! A declared write root a confined child can replace.
//!
//! A write root inside the workspace sits in a directory the child may write,
//! so one spawn can swap it for a link. The next spawn must not grant write
//! wherever that link points.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::{Path, PathBuf};

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};

fn writing_policy(workspace: &Path, write_root: &Path) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([
            ("process".to_string(), vec!["run".to_string()]),
            (
                "workspace".to_string(),
                vec!["read_text".to_string(), "write_text".to_string()],
            ),
        ]),
        workspace_roots: vec![workspace.display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        sandbox_profile: SandboxProfile::OsHardened,
        ..CapabilityPolicy::default()
    };
    policy.process_sandbox.write_roots = vec![write_root.display().to_string()];
    policy
}

/// Run `script` through the confined spawn path a process capability uses.
fn shell(policy: &CapabilityPolicy, script: &str) -> bool {
    push_execution_policy(policy.clone());
    let output =
        crate::stdlib::sandbox::std_command_for("/bin/sh", &["-c".to_string(), script.to_string()])
            .and_then(|mut command| {
                command
                    .output()
                    .map_err(|error| crate::value::VmError::Runtime(error.to_string()))
            });
    pop_execution_policy();
    match output {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!("{script}: {}", String::from_utf8_lossy(&output.stderr));
            false
        }
        Err(error) => {
            eprintln!("{script}: {error}");
            false
        }
    }
}

/// A workspace under the working directory and a directory under `HOME`
/// standing in for the user's git configuration: neither under a shared temp
/// dir a preset might grant.
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

/// Child one replaces the declared write root `<ws>/target` with a link to a
/// directory outside every grant. Child two, under the same policy, must not
/// be able to write through it.
#[test]
fn a_write_root_a_child_replaced_with_a_link_grants_nothing_outside() {
    if !crate::stdlib::sandbox::active_backend_filesystem_available() {
        crate::sandbox_host::boundary_not_measured(
            "a_write_root_a_child_replaced_with_a_link_grants_nothing_outside",
            "this host has no filesystem sandbox",
        );
        return;
    }
    let Some((workspace, outside)) = workspace_and_outside() else {
        return;
    };
    let ws = std::fs::canonicalize(workspace.path()).unwrap();
    let outside = std::fs::canonicalize(outside.path()).unwrap();
    let target = ws.join("target");
    std::fs::create_dir(&target).unwrap();
    let policy = writing_policy(&ws, &target);
    let planted = outside.join("planted");

    // Controls: the child writes its write root, and cannot write outside.
    assert!(
        shell(
            &policy,
            &format!("echo ok > '{}'", target.join("ok").display())
        ),
        "the child could not write its own write root, so nothing was measured"
    );
    assert!(!shell(
        &policy,
        &format!("echo x > '{}'", planted.display())
    ));
    assert!(!planted.exists());

    // Child one swaps the write root for a link out of the workspace.
    assert!(
        shell(
            &policy,
            &format!(
                "rm -rf '{target}' && ln -s '{outside}' '{target}'",
                target = target.display(),
                outside = outside.display()
            )
        ),
        "the child could not replace its write root"
    );
    assert!(target.is_symlink());

    // Child two writes through it.
    shell(
        &policy,
        &format!("echo pwned > '{}'", target.join("planted").display()),
    );
    assert!(
        !planted.exists(),
        "a write root replaced with a link granted write at {}",
        outside.display()
    );
}

/// A read root the workspace reaches through a link it holds, as a checkout
/// can ship one, grants no read wherever the link points.
#[test]
fn a_read_root_reached_through_a_workspace_link_grants_nothing_outside() {
    if !crate::stdlib::sandbox::active_backend_filesystem_available() {
        return;
    }
    let Some((workspace, outside)) = workspace_and_outside() else {
        return;
    };
    let ws = std::fs::canonicalize(workspace.path()).unwrap();
    let outside = std::fs::canonicalize(outside.path()).unwrap();
    std::fs::write(outside.join("secret"), "value").unwrap();
    let vendor = ws.join("vendor");
    std::os::unix::fs::symlink(&outside, &vendor).unwrap();
    std::fs::create_dir(ws.join("target")).unwrap();
    let mut policy = writing_policy(&ws, &ws.join("target"));
    let read = format!("cat '{}'", vendor.join("secret").display());
    if shell(&policy, &read) {
        crate::sandbox_host::boundary_not_measured(
            "a_read_root_reached_through_a_workspace_link_grants_nothing_outside",
            "this backend lets a child read the outside directory without a grant",
        );
        return;
    }
    // Control: the same directory named directly as a read root is readable,
    // so a refusal below is the link's, not the backend ignoring read roots.
    policy.process_sandbox.read_roots = vec![outside.display().to_string()];
    let direct = format!("cat '{}'", outside.join("secret").display());
    assert!(
        shell(&policy, &direct),
        "a read root named directly was not readable, so nothing was measured"
    );
    policy.process_sandbox.read_roots = vec![vendor.display().to_string()];
    assert!(
        !shell(&policy, &read),
        "a read root reached through a workspace link granted read at {}",
        outside.display()
    );
}

/// A write root on or over the user's own git, gh or ssh configuration is not
/// granted.
#[test]
fn a_write_root_over_the_users_configuration_is_not_granted() {
    let Some(home) = crate::stdlib::sandbox::sandbox_user_home_dir() else {
        return;
    };
    let workspace = tempfile::tempdir().unwrap();
    let ws = std::fs::canonicalize(workspace.path()).unwrap();
    for root in [
        home.join(".ssh"),
        home.join(".config/git/hooks"),
        home.clone(),
    ] {
        let policy = writing_policy(&ws, &root);
        let granted = crate::stdlib::sandbox::process_sandbox_policy_write_roots(&policy);
        assert!(
            granted.is_empty(),
            "{} was granted: {granted:?}",
            root.display()
        );
    }
}
