use crate::test_util;

#[test]
fn check_requires_explicit_trusted_host_dispatch_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("route.harn"),
        "pub fn route(payload) { return host_call(\"cloud.echo\", payload) }\n",
    )
    .expect("write route");

    let ordinary = test_util::process::harn_e2e_command()
        .arg("check")
        .args(["--preflight", "off", "route.harn"])
        .current_dir(temp.path())
        .env("HARN_CHECK_RESULT_CACHE", "0")
        .output()
        .expect("run ordinary check");
    assert!(
        !ordinary.status.success(),
        "ordinary test unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&ordinary.stdout),
        String::from_utf8_lossy(&ordinary.stderr)
    );
    let ordinary_stderr = String::from_utf8_lossy(&ordinary.stderr);
    assert!(
        ordinary_stderr.contains("HARN-NAM-002"),
        "{ordinary_stderr}"
    );
    assert!(ordinary_stderr.contains("host_call"), "{ordinary_stderr}");

    let trusted = test_util::process::harn_e2e_command()
        .arg("check")
        .args([
            "--trusted-host-dispatch",
            "--preflight",
            "off",
            "route.harn",
        ])
        .current_dir(temp.path())
        .env("HARN_CHECK_RESULT_CACHE", "0")
        .output()
        .expect("run trusted check");
    assert!(
        trusted.status.success(),
        "trusted check failed:\n{}",
        String::from_utf8_lossy(&trusted.stderr)
    );
}

#[test]
fn test_requires_explicit_trusted_host_dispatch_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("route_test.harn"),
        r#"
import { with_scenario } from "std/testing"

pipeline test_route(harness: Harness, _task) {
  const value = with_scenario(
    harness,
    {
      capabilities: [
        {
          capability: "cloud",
          method: "echo",
          result: "fixture",
          unregistered_ok: true,
        },
      ],
    },
    { _ -> host_call("cloud.echo", {}) },
  )
  assert_eq(value, "fixture")
}
"#,
    )
    .expect("write test");

    let ordinary = test_util::process::harn_e2e_command()
        .arg("test")
        .arg("route_test.harn")
        .current_dir(temp.path())
        .output()
        .expect("run ordinary test");
    assert!(
        !ordinary.status.success(),
        "ordinary test unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&ordinary.stdout),
        String::from_utf8_lossy(&ordinary.stderr)
    );
    assert!(
        String::from_utf8_lossy(&ordinary.stdout).contains("host_call")
            || String::from_utf8_lossy(&ordinary.stderr).contains("host_call")
    );

    let trusted = test_util::process::harn_e2e_command()
        .arg("test")
        .arg("--trusted-host-dispatch")
        .arg("route_test.harn")
        .current_dir(temp.path())
        .output()
        .expect("run trusted test");
    assert!(
        trusted.status.success(),
        "trusted test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&trusted.stdout),
        String::from_utf8_lossy(&trusted.stderr)
    );
}
