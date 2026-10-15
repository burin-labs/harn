use std::io::Write;
use std::process::{Command, Stdio};

use harn_vm::secrets::{MemorySecretProvider, ParentSecretHandoff, SecretId};

#[test]
fn parent_secrets_reach_sequential_and_parallel_imported_user_tests() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("helper.harn"),
        r#"pub fn prove_secret(harness: Harness) {
  assert(harness.secrets.read("fixture/github-token") == "synthetic-fixture-token")
  for name in ["fixture/absent", "fixture/ungranted"] {
    let refused = false
    try {
      harness.secrets.read(name)
    } catch (err) {
      refused = err.category == "not_found"
    }
    assert(refused, "an absent or ungranted name must stay unavailable")
  }
}
"#,
    )
    .unwrap();
    let child_control = if cfg!(unix) {
        r#"const child = harness.process.exec("env").stdout
  assert(!child.contains("synthetic-fixture-token"), "parent grants must not enter child environments")"#
    } else {
        ""
    };
    let suite = root.join("test_parent.harn");
    std::fs::write(
        &suite,
        format!(
            r#"import {{ prove_secret }} from "./helper.harn"
pipeline test_first(harness: Harness) {{ prove_secret(harness); {child_control} }}
pipeline test_second(harness: Harness) {{ prove_secret(harness) }}
"#
        ),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for parallel in [false, true] {
        let id = SecretId::new("fixture", "github-token");
        let provider =
            MemorySecretProvider::new("fixture").with_secret(id.clone(), "synthetic-fixture-token");
        let handoff = runtime
            .block_on(ParentSecretHandoff::capture(&provider, [id]))
            .unwrap();
        let mut frame = Vec::new();
        handoff.write_to(&mut frame).unwrap();
        let report = root.join("report.json");
        let mut command = Command::new(super::binary_path());
        command
            .current_dir(root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("HARN_SECRET_PROVIDERS", "must-not-be-consulted")
            .env("HARN_SECRET_FIXTURE_UNGRANTED", "ambient-only")
            .args(["test", "--parent-secret-stdin", "--json-out"])
            .arg(&report)
            .arg(&suite)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        if parallel {
            command.args(["--parallel", "--jobs", "2"]);
        }
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(&frame).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "parallel={parallel}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["summary"]["passed"], 2);
        assert_eq!(report["summary"]["failed"], 0);
        assert_eq!(report["summary"]["skipped"], 0);
        assert_eq!(report["summary"]["total"], 2);
    }
}
