//! Write grants in the rendered macOS profile come only from named roots.
//!
//! `(allow file-write*)` with no filter is an unqualified grant. A policy that
//! allows workspace writes but names no preset, policy, or cache write roots
//! must still confine the child to its workspace roots.

use super::*;

fn system_runtime_only_policy(workspace: &str) -> CapabilityPolicy {
    CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from([(
            "workspace".to_string(),
            vec!["read_text".to_string(), "write_text".to_string()],
        )]),
        workspace_roots: vec![workspace.to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: Box::new(crate::orchestration::ProcessSandboxPolicy {
            presets: Some(vec![ProcessSandboxPreset::SystemRuntime]),
            ..Default::default()
        }),
        ..CapabilityPolicy::default()
    }
}

/// Every filesystem location an `(allow file-write* ...)` rule names, except
/// the standard I/O devices every profile carries.
fn write_grants(profile: &str) -> Vec<String> {
    let mut grants = Vec::new();
    for line in profile
        .lines()
        .filter(|line| line.starts_with("(allow file-write*"))
    {
        assert!(
            line.contains("(subpath ") || line.contains("(literal "),
            "an unfiltered write rule grants every path: {line}"
        );
        for piece in line.split("(subpath \"").skip(1) {
            let path = piece.split('"').next().unwrap_or_default();
            if path != "/dev/fd" {
                grants.push(path.to_string());
            }
        }
    }
    grants.sort();
    grants.dedup();
    grants
}

#[test]
fn empty_write_root_lists_grant_no_write_beyond_the_workspace() {
    let policy = system_runtime_only_policy("/ws");
    let profile = render_profile(&policy);

    assert!(
        !profile.lines().any(|line| line == "(allow file-write*)"),
        "empty write-root lists must not render an unqualified write grant: {profile}"
    );
    assert_eq!(write_grants(&profile), vec!["/ws".to_string()], "{profile}");
}

#[test]
fn one_write_root_grants_exactly_that_root_and_the_workspace() {
    let mut policy = system_runtime_only_policy("/ws");
    policy.process_sandbox.write_roots = vec!["/opt/vendor-cache".to_string()];
    let profile = render_profile(&policy);

    assert!(
        profile
            .lines()
            .any(|line| line == "(allow file-write* (subpath \"/opt/vendor-cache\"))"),
        "the named write root should be granted: {profile}"
    );
    assert_eq!(
        write_grants(&profile),
        vec!["/opt/vendor-cache".to_string(), "/ws".to_string()],
        "{profile}"
    );
}

/// A real confined child: the in-workspace write proves the child ran, and the
/// write outside every granted root must be refused. Naming the outside
/// directory as a write root must then admit the same write, so the refusal
/// is the root list's doing and not an unrelated failure.
#[test]
fn a_live_child_with_no_write_roots_cannot_write_outside_the_workspace() {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return;
    }
    // Keep the outside directory out of every temp-dir grant.
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME for the outside directory");
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir_in(home).expect("outside dir");
    let workspace_path = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
    let outside_path = std::fs::canonicalize(outside.path()).expect("canonical outside");

    let write = |policy: &CapabilityPolicy, target: &Path| {
        let config = ProcessCommandConfig {
            cwd: Some(workspace_path.clone()),
            ..Default::default()
        };
        Backend::run_to_output(
            "/bin/sh",
            &[
                "-c".to_string(),
                "echo marker > \"$1\"".to_string(),
                "sh".to_string(),
                target.display().to_string(),
            ],
            &config,
            policy,
            SandboxProfile::Worktree,
        )
        .expect("spawn confined shell")
    };

    let policy = system_runtime_only_policy(&workspace_path.display().to_string());
    let inside_marker = workspace_path.join("inside-marker");
    let inside = write(&policy, &inside_marker);
    assert!(
        inside.status.success() && inside_marker.exists(),
        "the workspace write must succeed: {}",
        String::from_utf8_lossy(&inside.stderr)
    );

    let outside_marker = outside_path.join("outside-marker");
    let refused = write(&policy, &outside_marker);
    assert!(
        !refused.status.success() && !outside_marker.exists(),
        "a write outside every granted root must be refused"
    );

    let mut granted = policy;
    granted.process_sandbox.write_roots = vec![outside_path.display().to_string()];
    let admitted = write(&granted, &outside_marker);
    assert!(
        admitted.status.success() && outside_marker.exists(),
        "a named write root must admit the same write: {}",
        String::from_utf8_lossy(&admitted.stderr)
    );
}
