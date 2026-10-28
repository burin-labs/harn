use super::*;

const HELPER_MODE: &str = "PROCESS_SANDBOX_TEST_HELPER";
const CANARY_NAME: &str = "PROCESS_SANDBOX_CONTROLLER_CANARY";
const CANARY: &str = "public-nonsecret-controller-canary";

// Only booleans and syscall status leave the reader. Never print the environment
// buffer, which could contain unrelated host secrets in a regressed build.
const READER: &str = r"
import ctypes, json, os, subprocess, sys, time
with open(sys.argv[1], 'w') as marker:
    marker.write('child reached')
libc = ctypes.CDLL(None, use_errno=True)
result = []
for selector in [38, 49]:
    for target, pid in [('parent', os.getppid()), ('self', os.getpid())]:
        mib = (ctypes.c_int * 3)(1, selector, pid)
        size = ctypes.c_size_t(1048576)
        buf = ctypes.create_string_buffer(size.value)
        status = libc.sysctl(mib, 3, buf, ctypes.byref(size), None, 0)
        fields = buf.raw[:min(size.value, len(buf))].split(b'\0')
        result.append({'selector': selector, 'target': target, 'status': status,
            'parent_canary': b'PROCESS_SANDBOX_CONTROLLER_CANARY=public-nonsecret-controller-canary' in fields,
            'self_canary': b'PROCESS_SANDBOX_CHILD_CANARY=public-nonsecret-child-canary' in fields})
assert time.monotonic() > 0
assert subprocess.check_output(['/bin/echo', 'child-command'], text=True).strip() == 'child-command'
print(json.dumps(result))
";

#[test]
fn confined_child_cannot_read_the_hosts_launch_environment() {
    if std::env::var_os(HELPER_MODE).is_none() {
        let name = "stdlib::sandbox::macos::parent_environment_tests::confined_child_cannot_read_the_hosts_launch_environment";
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", name, "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", std::env::var_os("HOME").expect("home"))
            .env(HELPER_MODE, "1")
            .env(CANARY_NAME, CANARY)
            .output()
            .expect("isolated parent test helper");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if Backend::filesystem_available() {
            "parent environment protected; self and child commands reached"
        } else {
            "unsupported host refusal reached; child did not start"
        };
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let marker = workspace.path().join("child-reached");
    let args = vec![
        "-c".to_string(),
        READER.to_string(),
        marker.display().to_string(),
    ];
    let direct = Command::new("/usr/bin/python3")
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(
            "PROCESS_SANDBOX_CHILD_CANARY",
            "public-nonsecret-child-canary",
        )
        .output()
        .expect("positive control");
    assert!(direct.status.success());
    assert!(marker.exists(), "positive control must start the reader");
    std::fs::remove_file(&marker).expect("clear the measured control marker");
    let before: serde_json::Value = serde_json::from_slice(&direct.stdout).expect("control status");
    let controls = before.as_array().expect("control inventory");
    assert_eq!(controls.len(), 4);
    for row in controls {
        assert_eq!(row["status"], 0, "control reader must execute");
        if row["target"] == "parent" {
            assert_eq!(
                row["parent_canary"], true,
                "both selectors must observe the known parent marker"
            );
        }
    }

    let mut policy = tests::macos_policy_with_workspace_ops(&[]);
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    policy.process_sandbox.allow_child_workspace_write = true;
    let config = ProcessCommandConfig {
        cwd: Some(workspace.path().to_path_buf()),
        closed_env: true,
        env: vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "PROCESS_SANDBOX_CHILD_CANARY".to_string(),
                "public-nonsecret-child-canary".to_string(),
            ),
        ],
        ..Default::default()
    };
    let output = Backend::run_to_output(
        "/usr/bin/python3",
        &args,
        &config,
        &policy,
        SandboxProfile::OsHardened,
    );
    if !Backend::filesystem_available() {
        let error = output.expect_err("unsupported host must refuse the confined child");
        let refusal = error
            .sandbox_mechanism_unavailable()
            .expect("typed mechanism refusal");
        // The refusal names the prerequisite that failed, not a missing binary.
        assert_eq!(
            *refusal,
            Backend::filesystem_unavailable(SandboxProfile::OsHardened)
        );
        assert!(!marker.exists(), "refused child must never start");
        println!("unsupported host refusal reached; child did not start");
        return;
    }
    let output = output.expect("confined reader reached");
    assert!(
        marker.exists(),
        "supported host must start the confined reader"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after: serde_json::Value = serde_json::from_slice(&output.stdout).expect("confined status");
    let rows = after.as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        assert_eq!(
            row["status"], 0,
            "reader must run rather than fail before reading"
        );
        assert_eq!(row["parent_canary"], false, "observed statuses: {after}");
        if row["target"] == "self" {
            assert_eq!(row["self_canary"], true);
        }
    }
    println!("parent environment protected; self and child commands reached");
}
