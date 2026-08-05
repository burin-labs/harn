use super::*;

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
