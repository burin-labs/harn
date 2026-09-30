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
const RESULT_PREFIX: &str = "NESTED_SANDBOX_RESULT ";

fn workspace_policy(workspace: &Path) -> CapabilityPolicy {
    CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.display().to_string()],
        ..CapabilityPolicy::default()
    }
}

/// The child side: spawn a shell under the policy and report the outcome.
fn run_child() {
    let workspace = PathBuf::from(std::env::var(WORKSPACE_ENV).expect("workspace"));
    let outside = PathBuf::from(std::env::var(OUTSIDE_ENV).expect("outside"));
    let policy = workspace_policy(&workspace);
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

/// Run `test` again under `outer_profile`, as the child side.
fn run_nested(test: &str, outer_profile: impl FnOnce(&Path, &Path) -> String) -> Nested {
    let workspace = tempfile::tempdir().expect("workspace");
    let exe = std::env::current_exe().expect("test binary");
    // Outside every root the policy writes, temp included: under the build
    // tree, which the outer profile only reads.
    let outside = tempfile::tempdir_in(build_root(&exe)).expect("outside");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let outside_path = outside.path().canonicalize().expect("canonical outside");
    let profile = outer_profile(&workspace_path, &exe);
    let output = Command::new("/usr/bin/sandbox-exec")
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

/// Harn's own profile for the workspace, plus reading the test binary's tree.
fn harn_profile(workspace: &Path, exe: &Path) -> String {
    format!(
        "{}(allow file-read* (subpath \"{}\"))\n",
        super::super::render_profile_for_program(&workspace_policy(workspace), "/bin/sh"),
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
        harn_profile,
    );
    assert_eq!(nested.report["spawned"], true, "{}", nested.report);
    assert!(nested.inside_written, "{}", nested.report);
    assert!(!nested.outside_written, "{}", nested.report);
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
        |_, _| "(version 1)(allow default)(deny network-outbound)".to_string(),
    );
    assert_eq!(nested.report["spawned"], false, "{}", nested.report);
    let error = nested.report["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("already inside another sandbox") && error.contains("writes to"),
        "{error}"
    );
    assert!(!nested.inside_written && !nested.outside_written);
}
