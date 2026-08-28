use super::*;

#[cfg(target_os = "macos")]
struct CurrentDirGuard(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore current dir");
    }
}

fn unset_ambient_egress_policy() -> [crate::env_guard::ScopedEnvVar; 5] {
    [
        crate::env_guard::ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV),
        crate::env_guard::ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV),
        crate::env_guard::ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV),
        crate::env_guard::ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV),
        crate::env_guard::ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV),
    ]
}

#[tokio::test]
async fn execute_run_can_install_an_egress_policy_without_network_authority() {
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _egress_environment = unset_ambient_egress_policy();
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
fn main(harness: Harness) {
  harness.net.egress_policy({default: "deny", allow: ["api.example.com"]})
  harness.stdio.println("policy-installed")
}
"#,
    )
    .expect("write script");

    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout, "policy-installed\n");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn execute_run_can_call_a_matching_http_mock_without_network_authority() {
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _egress_environment = unset_ambient_egress_policy();
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let script = temp.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
fn main(harness: Harness) {
  const url = "https://api.example.com/value"
  harness.testing.http_mock("GET", url, {status: 200, body: "fixture", headers: {}})
  const response = harness.net.get(url)
  harness.stdio.println(response.body)
}
"#,
    )
    .expect("write script");

    let outcome = execute_run(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout, "fixture\n");
    harn_vm::reset_thread_local_state();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn canonical_run_allows_tcp_loopback_but_denies_remote_egress() {
    let _cwd_lock = crate::tests::common::cwd_lock::lock_cwd_async().await;
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _egress_environment = unset_ambient_egress_policy();
    harn_vm::reset_thread_local_state();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"));
    let hostile_cwd = tempfile::Builder::new()
        .prefix("harn-hostile-cwd-")
        .tempdir_in(home)
        .expect("hostile cwd outside the user-temp sandbox preset");
    let outside_file = hostile_cwd.path().join("denied.txt");
    std::fs::write(&outside_file, "outside").expect("write outside fixture");
    let script = temp.path().join("main.harn");
    let python = r#"import os
import socket
print('CWD=' + os.getcwd())
try:
    open(__import__('sys').argv[1], 'rb').read()
except PermissionError:
    print('FILESYSTEM_DENIED')
else:
    raise SystemExit('outside filesystem unexpectedly allowed')
for family, address in ((socket.AF_INET, ('127.0.0.1', 0)), (socket.AF_INET6, ('::1', 0))):
    server = socket.socket(family, socket.SOCK_STREAM)
    server.bind(address)
    server.listen(1)
    client = socket.socket(family, socket.SOCK_STREAM)
    client.connect(server.getsockname())
    accepted, _ = server.accept()
    client.sendall(b'ok')
    assert accepted.recv(2) == b'ok'
    accepted.close()
    client.close()
    server.close()
print('LOOPBACK_OK')
remote = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
remote.settimeout(1)
try:
    remote.connect(('192.0.2.1', 9))
except PermissionError:
    print('REMOTE_DENIED')
else:
    raise SystemExit('remote network unexpectedly allowed')"#;
    let source = format!(
        r#"
fn main(harness: Harness) -> int {{
  const child = harness.process.run({{
    program: "/usr/bin/python3",
    args: ["-c", {python}, {outside_file}],
  }})
  harness.stdio.print(child.stdout)
  harness.stdio.eprint(child.stderr)
  return child.exit_code
}}
"#,
        python = serde_json::to_string(python).expect("encode Python fixture"),
        outside_file =
            serde_json::to_string(&outside_file.to_string_lossy()).expect("encode outside path"),
    );
    std::fs::write(&script, source).expect("write script");

    let original_cwd = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(hostile_cwd.path()).expect("enter hostile ambient cwd");
    let cwd_guard = CurrentDirGuard(original_cwd);

    let outcome = execute_run_with_sandbox_options(
        &script.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::default()
            .with_workspace_root(temp.path())
            .with_process_loopback(true),
    )
    .await;
    drop(cwd_guard);

    assert_eq!(
        outcome.exit_code, 0,
        "stderr:\n{}\nstdout:\n{}",
        outcome.stderr, outcome.stdout
    );
    let workspace = std::fs::canonicalize(temp.path()).expect("canonical workspace");
    assert_eq!(
        outcome.stdout,
        format!(
            "CWD={}\nFILESYSTEM_DENIED\nLOOPBACK_OK\nREMOTE_DENIED\n",
            workspace.display()
        )
    );
    harn_vm::reset_thread_local_state();
}
