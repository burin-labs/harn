//! A selected executable must still name the same file when a child changes cwd.

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

#[test]
fn relative_path_entry_survives_the_project_working_directory() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("relative-bin")).unwrap();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    std::os::unix::fs::symlink(
        harn_e2e_binary(),
        root.path().join("relative-bin/quality-probe"),
    )
    .unwrap();
    std::os::unix::fs::symlink(harn_e2e_binary(), root.path().join("quality-probe")).unwrap();
    std::fs::write(
        root.path().join("probe.harn"),
        r#"fn main(harness: Harness) {
  const selected = harness.process.find_executable("quality-probe")
  assert(selected != nil, "the executable in the relative PATH entry must be selected")
  harness.stdio.println("selected=" + selected)
  assert_eq(selected, path_join(harness.fs.cwd(), argv[0]), "selection must remain absolute without resolving the symlink")
  const result = harness.process.run({program: selected, args: ["--version"], cwd: "workspace"})
  assert(result.success, "the selected executable must run from the project directory")
  assert(result.stdout.contains("harn"), "the selected child must actually execute")
  harness.stdio.println("relative-path-child-reached")
}
"#,
    )
    .unwrap();

    for (entry, expected) in [
        ("relative-bin", "relative-bin/quality-probe"),
        (".", "quality-probe"),
        ("", "quality-probe"),
    ] {
        let output = harn_e2e_command()
            .current_dir(root.path())
            .env("PATH", format!("{entry}:/usr/bin:/bin"))
            .env("HARN_LLM_CALLS_DISABLED", "1")
            .args(["run", "--no-sandbox", "probe.harn", "--", expected])
            .output()
            .expect("run Harn against a relative PATH entry");
        assert!(
            output.status.success(),
            "PATH entry {entry:?}: selected executable failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("relative-path-child-reached"));
    }
}
