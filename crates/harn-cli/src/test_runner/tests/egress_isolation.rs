//! Egress policy isolation across the test runner's parallel pipelines.
//!
//! These cases assert what a suite's egress configuration composes to when
//! several pipelines run at once: an environment policy and a script policy
//! must compose rather than one silently refusing the other, and one
//! pipeline's HTTP mocks must not reach another's.

use super::*;

#[tokio::test]
async fn parallel_pipelines_isolate_egress_policy_and_http_mocks() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _egress_env = [
        harn_vm::egress::HARN_EGRESS_ALLOW_ENV,
        harn_vm::egress::HARN_EGRESS_DENY_ENV,
        harn_vm::egress::HARN_EGRESS_DEFAULT_ENV,
        harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV,
        harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV,
    ]
    .map(ScopedEnvVar::unset);
    let temp = TempTestDir::new();
    let source = (0..32)
        .map(|index| {
            format!(
                r#"
pipeline test_policy_{index}(harness: Harness, _task) {{
  const url = "https://case-{index}.example.test/data"
  harness.net.egress_policy({{default: "deny", allow: ["case-{index}.example.test"]}})
  harness.testing.http_mock("GET", url, {{status: 200, body: "case-{index}", headers: {{}}}})
  const response = harness.net.get(url)
  assert_eq(response.body, "case-{index}")
}}
"#
            )
        })
        .collect::<String>();
    temp.write("suite/test_egress_parallel.harn", &source);

    let opts = RunOptions {
        parallel: true,
        jobs: Some(8),
        ..RunOptions::new(5_000)
    };
    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(
        summary.failed,
        0,
        "parallel egress state leaked: {:?}",
        summary
            .results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| (&result.name, &result.error))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 32);
}

/// harn#7613. This test previously asserted the opposite: that a pipeline
/// configuring its own egress policy under an ambient `HARN_EGRESS_*` setting
/// FAILS with "policy already configured from environment". That behavior made
/// an operator's environment variable break every unrelated connector and
/// conformance suite before the suite's own behavior was ever exercised, and the
/// failure read as a product defect in whatever connector happened to run. The
/// contract is now composition, so the assertion is inverted.
///
/// The connector-shaped pipeline must reach connector behavior with the
/// environment policy present, and the receipt must name both contributions.
#[tokio::test]
async fn environment_and_script_egress_policy_compose_without_failing_the_suite() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _allow = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV);
    let _deny = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV);
    let _default = ScopedEnvVar::set(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV, "deny");
    let _block_private = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV);
    let _allow_loopback = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_egress_environment.harn",
        r#"
pipeline test_environment_one(harness: Harness, _task) {
  const receipt = harness.net.egress_policy({allow: ["api.example.test"], default: "deny"})
  assert(contains(to_string(receipt.sources), "environment"), "receipt names the environment")
  assert(contains(to_string(receipt.sources), "stdlib"), "receipt names the script")
  harness.testing.http_mock("GET", "https://api.example.test/ok", {status: 200, body: "reached"})
  const response = harness.net.get("https://api.example.test/ok")
  assert_eq(response.body, "reached")
}

pipeline test_environment_two(harness: Harness, _task) {
  const receipt = harness.net.egress_policy({allow: ["api.example.test"], default: "deny"})
  assert_eq(receipt.default, "deny")
  harness.testing.http_mock("GET", "https://api.example.test/ok", {status: 200, body: "reached"})
  const response = harness.net.get("https://api.example.test/ok")
  assert_eq(response.body, "reached")
}
"#,
    );
    let opts = RunOptions {
        parallel: true,
        jobs: Some(2),
        ..RunOptions::new(5_000)
    };

    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(
        summary.failed,
        0,
        "an ambient egress policy must not fail a suite that configures its own: {:?}",
        summary
            .results
            .iter()
            .map(|result| (&result.name, &result.error))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 2);
}

/// Direction control for the test above: the identical suite with no ambient
/// egress configuration at all. Without this, a green run above could mean the
/// environment policy was silently ignored rather than composed.
#[tokio::test]
async fn script_egress_policy_reaches_connector_behavior_without_an_environment_policy() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _allow = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV);
    let _deny = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV);
    let _default = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV);
    let _block_private = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV);
    let _allow_loopback = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_egress_no_environment.harn",
        r#"
pipeline test_script_only(harness: Harness, _task) {
  const receipt = harness.net.egress_policy({allow: ["api.example.test"], default: "deny"})
  assert_eq(len(receipt.sources), 1)
  assert(contains(to_string(receipt.sources), "stdlib"), "only the script contributed")
  harness.testing.http_mock("GET", "https://api.example.test/ok", {status: 200, body: "reached"})
  const response = harness.net.get("https://api.example.test/ok")
  assert_eq(response.body, "reached")
}
"#,
    );
    let opts = RunOptions::new(5_000);

    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(
        summary.failed,
        0,
        "{:?}",
        summary
            .results
            .iter()
            .map(|result| (&result.name, &result.error))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 1);
}

/// The variable named in the report. A connector suite must reach its own
/// behavior with `HARN_EGRESS_BLOCK_PRIVATE` exported, and the composed policy
/// must keep the operator's private-address block rather than let the script
/// switch it off.
#[tokio::test]
async fn block_private_environment_policy_composes_with_a_connector_script_policy() {
    let _env_guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
    let _allow = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_ENV);
    let _deny = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DENY_ENV);
    let _default = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_DEFAULT_ENV);
    let _block_private =
        ScopedEnvVar::set(harn_vm::egress::HARN_EGRESS_BLOCK_PRIVATE_ENV, "private");
    let _allow_loopback = ScopedEnvVar::unset(harn_vm::egress::HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let temp = TempTestDir::new();
    temp.write(
        "suite/test_egress_block_private.harn",
        r#"
pipeline test_connector_policy(harness: Harness, _task) {
  const receipt = harness.net.egress_policy(
    {allow: ["api.example.test"], default: "deny", block_private: "off"},
  )
  assert(contains(to_string(receipt.sources), "environment"), "receipt names the environment")
  assert(contains(to_string(receipt.sources), "stdlib"), "receipt names the script")
  assert_eq(receipt.block_private, "private")
  assert_eq(receipt.default, "deny")
}
"#,
    );
    let opts = RunOptions::new(5_000);

    let summary = run_tests_with_options(&temp.path().join("suite"), &opts).await;

    assert_eq!(
        summary.failed,
        0,
        "HARN_EGRESS_BLOCK_PRIVATE must not fail a connector policy suite: {:?}",
        summary
            .results
            .iter()
            .map(|result| (&result.name, &result.error))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.passed, 1);
}
