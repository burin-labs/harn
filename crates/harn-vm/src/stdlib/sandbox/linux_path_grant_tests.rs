//! Live Landlock falsifiers for PATH-derived grants (harn#8998).

use super::tests::live_landlock_available;
use super::*;

/// harn#8998. A tool first on the child's `PATH` but outside every preset root
/// was refused: setup-node's `node` under a self-hosted runner's tool cache
/// exited 126 on every sandboxed call. Three live cases share one fixture:
///
/// 1. The tool runs by bare name when its directory is on `PATH`.
/// 2. The same file stays refused when its directory is NOT on `PATH`. This is
///    what keeps case 1 from passing because the fixture happens to sit under
///    some other grant.
/// 3. With `~/.cargo/bin` on `PATH`, `~/.cargo/credentials.toml` stays
///    unreadable: the credential denylist beats the PATH grant.
#[test]
fn a_tool_on_path_runs_confined_while_one_off_path_and_credentials_stay_denied() {
    use std::os::unix::fs::PermissionsExt;
    if !live_landlock_available("path-entry-grant") {
        return;
    }
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let root = tempfile::TempDir::new().expect("fixture root");
    let root_path = root.path().canonicalize().expect("canonical");
    let workspace = root_path.join("workspace");
    let tool_bin = root_path.join("toolcache/node/24.0.0/x64/bin");
    let home = root_path.join("home");
    let cargo_bin = home.join(".cargo/bin");
    for dir in [&workspace, &tool_bin, &cargo_bin] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let tool = tool_bin.join("harn-path-probe");
    let true_bin = std::fs::canonicalize("/bin/true").expect("a host true binary");
    std::fs::copy(&true_bin, &tool).expect("stage the probe tool");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let credentials = home.join(".cargo/credentials.toml");
    std::fs::write(&credentials, "token = \"PATH-GRANT-SECRET\"\n").expect("credentials");

    let previous_home = std::env::var_os("HOME");
    let previous_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    let run = |path: String, script: String| {
        std::env::set_var("PATH", path);
        crate::orchestration::push_execution_policy(CapabilityPolicy {
            workspace_roots: vec![workspace.display().to_string()],
            sandbox_profile: SandboxProfile::Worktree,
            process_sandbox: Box::new(crate::orchestration::ProcessSandboxPolicy {
                // Not UserTemp: the fixture lives under the temp directory,
                // and that preset would grant it for an unrelated reason.
                presets: Some(vec![ProcessSandboxPreset::DeveloperToolchains]),
                ..Default::default()
            }),
            ..CapabilityPolicy::default()
        });
        let output = crate::stdlib::sandbox::command_output(
            "/bin/sh",
            &["-c".to_string(), script],
            &crate::stdlib::sandbox::ProcessCommandConfig {
                cwd: Some(workspace.clone()),
                ..Default::default()
            },
        );
        crate::orchestration::pop_execution_policy();
        output
    };
    let on_path = run(
        format!("{}:/usr/bin:/bin", tool_bin.display()),
        "harn-path-probe && echo PATH-PROBE-RAN".to_string(),
    );
    let off_path = run(
        "/usr/bin:/bin".to_string(),
        format!("{} && echo PATH-PROBE-RAN", tool.display()),
    );
    let credential_read = run(
        format!("{}:/usr/bin:/bin", cargo_bin.display()),
        format!("cat {}; echo CREDENTIAL-PROBE-RAN", credentials.display()),
    );
    match previous_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    let on_path = on_path.expect("the on-PATH spawn is prepared");
    assert!(
        on_path.status.success()
            && String::from_utf8_lossy(&on_path.stdout).contains("PATH-PROBE-RAN"),
        "a tool whose directory is on PATH must run confined: {on_path:?}"
    );
    // The runtime reports an OS-sandbox refusal as a typed error rather than a
    // failed exit, so either shape counts as refused; a successful run does not.
    match off_path {
        Err(error) => assert!(
            format!("{error:?}").contains("denied by the OS sandbox"),
            "the off-PATH spawn failed for a reason other than the sandbox: {error:?}"
        ),
        Ok(output) => assert!(
            !output.status.success()
                && !String::from_utf8_lossy(&output.stdout).contains("PATH-PROBE-RAN"),
            "the same file off PATH must stay refused, or case 1 proves nothing: {output:?}"
        ),
    }
    // The refused read surfaces as a typed error that names the denylist; an
    // output is acceptable only if the probe ran and the secret never printed.
    match credential_read {
        Err(error) => assert!(
            format!("{error:?}").contains("is on the credential denylist"),
            "the credential read failed for a reason other than the denylist: {error:?}"
        ),
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("CREDENTIAL-PROBE-RAN") && !stdout.contains("PATH-GRANT-SECRET"),
                "~/.cargo/bin on PATH must not open ~/.cargo/credentials.toml: {output:?}"
            );
        }
    }
}
