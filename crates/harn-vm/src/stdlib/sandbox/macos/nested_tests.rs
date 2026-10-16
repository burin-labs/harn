//! A confined spawn from a process that is already sandboxed.
//!
//! A test process is not sandboxed, so each case re-runs this test binary
//! under `sandbox-exec` with an outer profile, and the re-run child performs
//! the confined spawn and prints what happened. The child side is the same
//! test function, selected by `NESTED_SANDBOX_CHILD`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::stdlib::sandbox::{ProcessCommandConfig, SandboxBackend};

const CHILD_ENV: &str = "NESTED_SANDBOX_CHILD";
const WORKSPACE_ENV: &str = "NESTED_SANDBOX_WORKSPACE";
const OUTSIDE_ENV: &str = "NESTED_SANDBOX_OUTSIDE";
const CASE_ENV: &str = "NESTED_SANDBOX_CASE";
const RESULT_PREFIX: &str = "NESTED_SANDBOX_RESULT ";

fn workspace_policy(workspace: &Path) -> CapabilityPolicy {
    CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.display().to_string()],
        ..CapabilityPolicy::default()
    }
}

/// What the child's policy adds to the workspace policy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Workspace,
    /// An explicit write grant for the outside fixture.
    GrantOutside,
    /// A read denial on a file inside the writable workspace.
    DenyInside,
    /// The same denial on a file that does not exist yet.
    DenyAbsent,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::GrantOutside => "grant_outside",
            Self::DenyInside => "deny_inside",
            Self::DenyAbsent => "deny_absent",
        }
    }

    fn from_env() -> Self {
        match std::env::var(CASE_ENV).as_deref() {
            Ok("grant_outside") => Self::GrantOutside,
            Ok("deny_inside") => Self::DenyInside,
            Ok("deny_absent") => Self::DenyAbsent,
            _ => Self::Workspace,
        }
    }
}

const DENIED_FILE: &str = "denied/secret.json";
const ABSENT_DENIED_FILE: &str = "denied/future.json";

fn case_policy(workspace: &Path, outside: &Path, case: Case) -> CapabilityPolicy {
    let mut policy = workspace_policy(workspace);
    match case {
        Case::Workspace => {}
        Case::GrantOutside => {
            policy.process_sandbox.write_roots = vec![outside.display().to_string()];
        }
        Case::DenyInside => {
            policy.process_sandbox.read_deny_roots =
                vec![workspace.join(DENIED_FILE).display().to_string()];
        }
        Case::DenyAbsent => {
            policy.process_sandbox.read_deny_roots =
                vec![workspace.join(ABSENT_DENIED_FILE).display().to_string()];
        }
    }
    policy
}

/// The child side: spawn a shell under the policy and report the outcome.
fn run_child() {
    let workspace = PathBuf::from(std::env::var(WORKSPACE_ENV).expect("workspace"));
    let outside = PathBuf::from(std::env::var(OUTSIDE_ENV).expect("outside"));
    let policy = case_policy(&workspace, &outside, Case::from_env());
    let script = format!(
        "echo in > '{}'; echo out > '{}'",
        workspace.join("inside.txt").display(),
        outside.join("outside.txt").display()
    );
    let config = ProcessCommandConfig {
        cwd: Some(workspace),
        ..ProcessCommandConfig::default()
    };
    let result = super::super::Backend::run_to_output(
        "/bin/sh",
        &["-c".to_string(), script],
        &config,
        &policy,
        SandboxProfile::Worktree,
    );
    let report = match result {
        Ok(output) => serde_json::json!({
            "spawned": true,
            "stderr": String::from_utf8_lossy(&output.stderr),
        }),
        Err(error) => serde_json::json!({"spawned": false, "error": error.to_string()}),
    };
    println!("{RESULT_PREFIX}{report}");
}

struct Nested {
    report: serde_json::Value,
    inside_written: bool,
    outside_written: bool,
}

impl Nested {
    fn refused_on_unsupported_host(&self) -> bool {
        let Some(refusal) = crate::sandbox_host::unavailable_worktree_sandbox() else {
            return false;
        };
        assert_eq!(self.report["spawned"], false, "{}", self.report);
        assert_eq!(
            self.report["error"],
            refusal.into_error().to_string(),
            "{}",
            self.report
        );
        assert!(!self.inside_written && !self.outside_written);
        true
    }
}

/// Run `test` again under `outer_profile`, as the child side, with the
/// child's policy shaped by `case`.
fn run_nested(
    test: &str,
    case: Case,
    outer_profile: impl FnOnce(&CapabilityPolicy, &Path) -> String,
) -> Nested {
    let workspace = tempfile::tempdir().expect("workspace");
    let exe = std::env::current_exe().expect("test binary");
    // Cargo targets may be inside a granted compiler cache. Use the owning
    // checkout's build fixtures, then prove this path is outside every grant.
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.build/test-fixtures");
    std::fs::create_dir_all(&fixture_root).expect("build fixture root");
    let outside = tempfile::tempdir_in(&fixture_root).expect("outside");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let outside_path = outside.path().canonicalize().expect("canonical outside");
    let denied = workspace_path.join(DENIED_FILE);
    std::fs::create_dir_all(denied.parent().unwrap()).expect("denied dir");
    std::fs::write(&denied, "synthetic-denied").expect("denied file");
    for root in super::writable_roots(&workspace_policy(&workspace_path)) {
        assert!(
            !outside_path.starts_with(&root),
            "outside fixture {} is inside writable grant {}",
            outside_path.display(),
            root.display()
        );
    }
    let policy = case_policy(&workspace_path, &outside_path, case);
    if case == Case::GrantOutside {
        assert!(
            super::writable_roots(&policy)
                .iter()
                .any(|root| outside_path.starts_with(root)),
            "the explicit grant did not make {} writable",
            outside_path.display()
        );
    }
    let profile = outer_profile(&policy, &exe);
    let output = Command::new("/usr/bin/sandbox-exec")
        .env(CASE_ENV, case.name())
        .arg("-p")
        .arg(&profile)
        .arg(&exe)
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env(WORKSPACE_ENV, &workspace_path)
        .env(OUTSIDE_ENV, &outside_path)
        .output()
        .expect("run the test binary under the outer profile");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = stdout
        .lines()
        // libtest prints the test's name on the same line first.
        .find_map(|line| line.split_once(RESULT_PREFIX).map(|(_, json)| json))
        .map(|json| serde_json::from_str(json).expect("child report"))
        .unwrap_or_else(|| {
            panic!(
                "the child printed no report; status={:?}\nstdout={stdout}\nstderr={}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        });
    Nested {
        report,
        inside_written: workspace_path.join("inside.txt").exists(),
        outside_written: outside_path.join("outside.txt").exists(),
    }
}

/// The build tree holding the test binary (`<target>/<profile>/deps/<exe>`).
fn build_root(exe: &Path) -> &Path {
    exe.ancestors().nth(3).unwrap_or(exe)
}

/// Harn's own profile for the policy, plus reading the test binary's tree.
fn harn_profile(policy: &CapabilityPolicy, exe: &Path) -> String {
    format!(
        "{}(allow file-read* (subpath \"{}\"))\n",
        super::super::render_profile_for_program(policy, "/bin/sh"),
        build_root(exe).display()
    )
}

/// A confined Harn inside Harn's own sandbox starts its child under the
/// inherited profile: the workspace write lands and the outside write is
/// still refused.
#[test]
fn a_child_inherits_an_outer_sandbox_as_strict_as_its_policy() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    let nested = run_nested(
        "stdlib::sandbox::macos::nested::tests::a_child_inherits_an_outer_sandbox_as_strict_as_its_policy",
        Case::Workspace,
        harn_profile,
    );
    if nested.refused_on_unsupported_host() {
        return;
    }
    assert_eq!(nested.report["spawned"], true, "{}", nested.report);
    assert!(nested.inside_written, "{}", nested.report);
    assert!(!nested.outside_written, "{}", nested.report);
}

/// The control for the refusal above: the same fixture, explicitly granted,
/// takes the write. Without it, an outside directory that nothing could write
/// would pass the strict case for a reason that has nothing to do with the
/// policy.
#[test]
fn an_explicitly_granted_outside_directory_takes_the_nested_write() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    let nested = run_nested(
        "stdlib::sandbox::macos::nested::tests::an_explicitly_granted_outside_directory_takes_the_nested_write",
        Case::GrantOutside,
        harn_profile,
    );
    if nested.refused_on_unsupported_host() {
        return;
    }
    assert_eq!(nested.report["spawned"], true, "{}", nested.report);
    assert!(nested.inside_written, "{}", nested.report);
    assert!(nested.outside_written, "{}", nested.report);
}

/// An outer sandbox that allows what the policy denies is not inherited: the
/// spawn is refused, naming what the outer sandbox allows.
#[test]
fn a_weaker_outer_sandbox_refuses_the_spawn_with_its_reason() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    let nested = run_nested(
        "stdlib::sandbox::macos::nested::tests::a_weaker_outer_sandbox_refuses_the_spawn_with_its_reason",
        Case::Workspace,
        |_, _| "(version 1)(allow default)(deny network-outbound)".to_string(),
    );
    if nested.refused_on_unsupported_host() {
        return;
    }
    assert_eq!(nested.report["spawned"], false, "{}", nested.report);
    let error = nested.report["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("already inside another sandbox") && error.contains("writes to"),
        "{error}"
    );
    assert!(!nested.inside_written && !nested.outside_written);
}

/// A read denial inside the writable workspace holds only while nothing on its
/// path can be renamed. Harn's own profile pins that path, so it is inherited;
/// the same profile without its unlink rules is refused for that read.
#[test]
fn an_outer_sandbox_must_pin_a_denial_inside_a_writable_root() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    let test = "stdlib::sandbox::macos::nested::tests::an_outer_sandbox_must_pin_a_denial_inside_a_writable_root";
    let pinned = run_nested(test, Case::DenyInside, harn_profile);
    if pinned.refused_on_unsupported_host() {
        return;
    }
    assert_eq!(pinned.report["spawned"], true, "{}", pinned.report);

    let unpinned = run_nested(test, Case::DenyInside, |policy, exe| {
        harn_profile(policy, exe)
            .lines()
            .filter(|line| !line.contains("file-write-unlink"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    assert_eq!(unpinned.report["spawned"], false, "{}", unpinned.report);
    let error = unpinned.report["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("reads of") && error.contains(DENIED_FILE),
        "{error}"
    );
}

/// A denied path that does not exist yet cannot be measured against the outer
/// sandbox, which answers "denied" for every absent path. The spawn is refused
/// even under Harn's own pinned profile, rather than inherited on an answer
/// that proves nothing.
#[test]
fn an_absent_denial_inside_a_writable_root_is_not_inherited() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    let nested = run_nested(
        "stdlib::sandbox::macos::nested::tests::an_absent_denial_inside_a_writable_root_is_not_inherited",
        Case::DenyAbsent,
        harn_profile,
    );
    if nested.refused_on_unsupported_host() {
        return;
    }
    assert_eq!(nested.report["spawned"], false, "{}", nested.report);
    let error = nested.report["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("reads of") && error.contains(ABSENT_DENIED_FILE),
        "{error}"
    );
}
