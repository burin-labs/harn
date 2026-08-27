use crate::test_util;

fn write_imported_host_dispatch_graph(root: &std::path::Path) {
    std::fs::create_dir_all(root).expect("create project");
    std::fs::write(
        root.join("helper.harn"),
        "pub fn dispatch(payload: dict) { return host_call(\"cloud.echo\", payload) }\n",
    )
    .expect("write helper");
    std::fs::write(
        root.join("route.harn"),
        "import { dispatch } from \"./helper\"\n\npub fn route(payload: dict) { return dispatch(payload) }\n",
    )
    .expect("write route");
}

#[test]
fn check_requires_explicit_trusted_host_dispatch_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("route.harn"),
        "pub fn route(payload: dict) { return host_call(\"cloud.echo\", payload) }\n",
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
fn manifest_authority_stays_inside_its_own_project_graph() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join(".git")).expect("project boundary");
    let trusted = temp.path().join("trusted");
    let ordinary = temp.path().join("ordinary");
    write_imported_host_dispatch_graph(&trusted);
    write_imported_host_dispatch_graph(&ordinary);
    std::fs::write(
        trusted.join("harn.toml"),
        "[check]\ntrusted_host_dispatch = true\n",
    )
    .expect("write trusted manifest");

    let allowed = test_util::process::harn_e2e_command()
        .arg("check")
        .args(["--preflight", "off", "helper.harn"])
        .current_dir(&trusted)
        .env("HARN_CHECK_RESULT_CACHE", "0")
        .output()
        .expect("check trusted graph");
    assert!(
        allowed.status.success(),
        "trusted graph failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );

    let denied = test_util::process::harn_e2e_command()
        .arg("check")
        .args(["--preflight", "off", "helper.harn"])
        .current_dir(&ordinary)
        .env("HARN_CHECK_RESULT_CACHE", "0")
        .output()
        .expect("check ordinary sibling graph");
    assert!(
        !denied.status.success(),
        "manifest authority escaped into a sibling project:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr)
    );
    let denied_stderr = String::from_utf8_lossy(&denied.stderr);
    assert!(denied_stderr.contains("host_call"), "{denied_stderr}");
    assert!(
        denied_stderr.contains("HARN-NAM-002") || denied_stderr.contains("not callable source API"),
        "{denied_stderr}"
    );
}

#[test]
fn test_requires_explicit_trusted_host_dispatch_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("route_test.harn"),
        r#"
import { with_scenario } from "std/testing"

pipeline test_route(harness: Harness, _task: unknown) {
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

/// `harn test` reads the same manifest declaration `harn check` and `harn lint`
/// read.
///
/// `[check].trusted_host_dispatch` is a project saying "I am a privileged
/// embedder". `check` and `lint` honored it and OR'd the CLI flag on top;
/// `test` read only the flag. A project that declared the authority once in its
/// manifest therefore still had every `host_call` refused under test, and the
/// only way to find that out was a whole suite failing. One key, three
/// commands, two answers.
#[test]
fn test_honors_trusted_host_dispatch_declared_in_the_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("harn.toml"),
        "[check]\ntrusted_host_dispatch = true\n",
    )
    .expect("write manifest");
    std::fs::write(
        temp.path().join("route_test.harn"),
        r#"
import { with_scenario } from "std/testing"

pipeline test_route(harness: Harness, _task: unknown) {
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

    // No `--trusted-host-dispatch` argument: the manifest alone must grant it.
    let declared = test_util::process::harn_e2e_command()
        .arg("test")
        .arg("route_test.harn")
        .current_dir(temp.path())
        .output()
        .expect("run declared test");
    assert!(
        declared.status.success(),
        "manifest-declared trusted host dispatch was ignored:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&declared.stdout),
        String::from_utf8_lossy(&declared.stderr)
    );
}
