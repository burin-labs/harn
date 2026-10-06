//! A confined command can confine its own children (harn#9454).
//!
//! The outer process is this test binary re-executed under an `os_hardened`
//! policy for a workspace `root`. Inside it, the fixture confines `/bin/sh` to
//! the narrower workspace `root/inner` and asks the shell to write one file in
//! each. The outer grant covers both, so the shell can be refused the outer
//! file only by a second Landlock layer of its own. The result line therefore
//! distinguishes the three outcomes that matter:
//!
//! - the nested command never ran (the ceiling withheld Landlock and the
//!   Bubblewrap fallback could not start), the defect this guards;
//! - it ran under the inherited domain alone (`outer-only-written`), which is
//!   weaker than the policy it asked for;
//! - it ran under both domains (`outer-only-denied`), the intersection.

#[cfg(target_os = "linux")]
const FIXTURE_ENV: &str = "HARN_NESTED_CONFINEMENT_FIXTURE";

#[cfg(target_os = "linux")]
fn os_hardened(
    workspace: &std::path::Path,
    read_only: Vec<String>,
) -> harn_vm::orchestration::CapabilityPolicy {
    harn_vm::orchestration::CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        read_only_roots: read_only,
        side_effect_level: Some("workspace_write".to_string()),
        sandbox_profile: harn_vm::orchestration::SandboxProfile::OsHardened,
        ..Default::default()
    }
}

/// Runs only inside the outer confinement, selected by `FIXTURE_ENV`.
#[cfg(target_os = "linux")]
#[test]
fn nested_confinement_fixture() {
    use harn_vm::orchestration::{pop_execution_policy, push_execution_policy};
    let Some(root) = std::env::var_os(FIXTURE_ENV) else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let inner = root.join("inner");
    // The positive control for the outer grant: this process writes the very
    // path its child must be refused.
    std::fs::write(root.join("outer-probe"), b"x").expect("the outer grant admits the root");

    push_execution_policy(os_hardened(&inner, Vec::new()));
    let mechanism = harn_vm::process_sandbox::active_backend_mechanism();
    let prepared = harn_vm::process_sandbox::std_command_for(
        "/bin/sh",
        &[
            "-c".to_string(),
            "printf ran; : > \"$1\" && printf :inner-written; \
             if ( : > \"$2\" ) 2>/dev/null; then printf :outer-only-written; \
             else printf :outer-only-denied; fi"
                .to_string(),
            "sh".to_string(),
            inner.join("ok").display().to_string(),
            root.join("outer-only").display().to_string(),
        ],
    );
    pop_execution_policy();
    let mut command = match prepared {
        Ok(command) => command,
        Err(error) => {
            println!("nested-result=refused:{mechanism:?}:{error}");
            return;
        }
    };
    command.current_dir(&inner);
    let output = command.output().expect("spawn the nested confined shell");
    println!(
        "nested-result={}|mechanism={mechanism:?}|status={:?}|stderr={}",
        String::from_utf8_lossy(&output.stdout),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim(),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_confined_command_stacks_its_own_confinement() {
    use harn_vm::orchestration::{pop_execution_policy, push_execution_policy};
    use harn_vm::process_sandbox::SandboxMechanism;
    if harn_vm::process_sandbox::active_backend_mechanism() != SandboxMechanism::LinuxLandlock {
        eprintln!("[nested-confinement] exercised=0: Landlock is not enforcing on this host");
        assert_ne!(
            std::env::var("HARN_REQUIRE_LANDLOCK_TESTS").as_deref(),
            Ok("1"),
            "this runner class declares Landlock enforcement"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("inner")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let exe_dir = exe.parent().unwrap().display().to_string();

    push_execution_policy(os_hardened(root.path(), vec![exe_dir]));
    let prepared = harn_vm::process_sandbox::std_command_for(
        &exe.display().to_string(),
        &[
            "--exact".to_string(),
            "nested_confinement::nested_confinement_fixture".to_string(),
            "--nocapture".to_string(),
            "--test-threads=1".to_string(),
        ],
    );
    pop_execution_policy();
    let mut command = prepared.expect("confine the outer fixture process");
    command
        .current_dir(root.path())
        .env(FIXTURE_ENV, root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn the outer fixture process");

    // The defect this guards hung rather than failed on a host with
    // Bubblewrap installed (harn#9351), so the wait is bounded here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "the nested confined command did not settle in 60 s: stdout={}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result = stdout
        .lines()
        .find_map(|line| line.split_once("nested-result=").map(|(_, result)| result))
        .unwrap_or_else(|| {
            panic!(
                "the fixture never reported: status={:?} stdout={stdout} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert!(
        result.starts_with("ran:inner-written:outer-only-denied|mechanism=LinuxLandlock|"),
        "a nested command must run under the intersection of both policies: {result}"
    );
    assert!(
        root.path().join("outer-probe").exists() && !root.path().join("outer-only").exists(),
        "the outer grant admitted the root, and only the inner layer refused the child"
    );
}
