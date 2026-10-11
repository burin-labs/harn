//! The write disposition from inside an already-sandboxed macOS process.
//!
//! An already-sandboxed Harn runs its children under its own inherited
//! profile, not the one it would render, so the disposition must ask that
//! profile. Each case re-runs this test binary under `sandbox-exec` with an
//! outer profile; the child side measures the disposition, attempts the write
//! through the real backend, and prints both.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::stdlib::sandbox::child_write::{child_write_disposition, ChildWriteDisposition};
use crate::stdlib::sandbox::{ProcessCommandConfig, SandboxBackend};

const CHILD_ENV: &str = "CHILD_WRITE_NESTED_CHILD";
const WORKSPACE_ENV: &str = "CHILD_WRITE_NESTED_WORKSPACE";
const ROOT_ENV: &str = "CHILD_WRITE_NESTED_ROOT";
const RESULT_PREFIX: &str = "CHILD_WRITE_NESTED_RESULT ";

fn policy(workspace: &Path, root: &Path) -> CapabilityPolicy {
    CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![workspace.display().to_string()],
        read_only_roots: vec![root.display().to_string()],
        ..CapabilityPolicy::default()
    }
}

/// The child side: measure, then try the write the measurement is about.
fn run_child() {
    let workspace = PathBuf::from(std::env::var(WORKSPACE_ENV).expect("workspace"));
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root"));
    let policy = policy(&workspace, &root);
    let target = root.join("f");
    let disposition = child_write_disposition(&policy, &target);
    let config = ProcessCommandConfig {
        cwd: Some(workspace),
        ..ProcessCommandConfig::default()
    };
    let spawned = super::Backend::run_to_output(
        "/bin/sh",
        &["-c".to_string(), format!("echo x > '{}'", target.display())],
        &config,
        &policy,
        SandboxProfile::OsHardened,
    )
    .is_ok();
    let report = serde_json::json!({
        "disposition": format!("{disposition:?}"),
        "missing": format!("{:?}", child_write_disposition(&policy, &root.join("missing"))),
        "nesting": format!("{:?}", super::nested::nesting(&policy)),
        "spawned": spawned,
    });
    println!("{RESULT_PREFIX}{report}");
}

/// Run `test` under the outer profile `extra` appends to Harn's own, and
/// return the child's report and whether the write landed.
fn run_nested(test: &str, extra: impl FnOnce(&Path) -> String) -> (serde_json::Value, bool) {
    let workspace = tempfile::tempdir().expect("workspace");
    let exe = std::env::current_exe().expect("test binary");
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.build/test-fixtures");
    std::fs::create_dir_all(&fixture_root).expect("build fixture root");
    let outside = tempfile::tempdir_in(&fixture_root).expect("root");
    let workspace_path = workspace.path().canonicalize().unwrap();
    let root = outside.path().canonicalize().unwrap();
    std::fs::write(root.join("f"), "reference").expect("seed the target");
    let build_root = exe.ancestors().nth(3).unwrap_or(&exe).to_path_buf();
    let profile = format!(
        "{}(allow file-read* (subpath \"{}\"))\n{}",
        super::render_profile_for_program(&policy(&workspace_path, &root), "/bin/sh"),
        build_root.display(),
        extra(&root)
    );
    let output = Command::new("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(&profile)
        .arg(&exe)
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env(WORKSPACE_ENV, &workspace_path)
        .env(ROOT_ENV, &root)
        .output()
        .expect("run the test binary under the outer profile");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = stdout
        .lines()
        .find_map(|line| line.split_once(RESULT_PREFIX).map(|(_, json)| json))
        .map(|json| serde_json::from_str(json).expect("child report"))
        .unwrap_or_else(|| {
            panic!(
                "the child printed no report; status={:?}\nstdout={stdout}\nstderr={}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let written = std::fs::read_to_string(root.join("f")).unwrap_or_default() != "reference";
    (report, written)
}

/// The inherited profile denies writes to the root: measured directly, so the
/// answer is read-only, and the write the child attempts is refused.
#[test]
fn an_inherited_profile_that_denies_the_root_is_read_only() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return;
    }
    let (report, written) = run_nested(
        "stdlib::sandbox::macos::child_write_nested_tests::an_inherited_profile_that_denies_the_root_is_read_only",
        |_| String::new(),
    );
    assert_eq!(
        report["disposition"],
        format!("{:?}", ChildWriteDisposition::EnforcedReadOnly),
        "{report}"
    );
    assert_eq!(report["nesting"], "Inherit", "{report}");
    assert!(!written, "{report}");
    // A target that does not exist cannot be asked about, so it measures
    // nothing even under a denying profile.
    assert_eq!(
        report["missing"],
        format!("{:?}", ChildWriteDisposition::Unknown),
        "{report}"
    );
}

/// The inherited profile allows writes to the root while the profile Harn
/// would render denies them. The child runs under the inherited one, so the
/// rendered answer would be a lie: the disposition is `Unknown`, and the
/// write really lands.
#[test]
fn an_inherited_profile_that_allows_the_root_is_unknown() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return run_child();
    }
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return;
    }
    let (report, written) = run_nested(
        "stdlib::sandbox::macos::child_write_nested_tests::an_inherited_profile_that_allows_the_root_is_unknown",
        |root| format!("(allow file-write* (subpath \"{}\"))\n", root.display()),
    );
    assert_eq!(
        report["disposition"],
        format!("{:?}", ChildWriteDisposition::Unknown),
        "{report}"
    );
    assert!(
        report["spawned"] == true && written,
        "the inherited profile allows the write, so it must land: {report}"
    );
}
