use super::{binary_path, git};
use std::path::Path;
use std::process::{Command, Output};

fn write(root: &Path, name: &str, source: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
    std::fs::write(path, source).expect("write fixture");
}

fn run(root: &Path, target: &str) -> (Output, serde_json::Value) {
    let output_dir = tempfile::tempdir().expect("report directory");
    let report_path = output_dir.path().join("report.json");
    let output = Command::new(binary_path())
        .current_dir(root)
        .args(["test", target, "--json-out"])
        .arg(&report_path)
        .output()
        .expect("run user tests");
    let report = serde_json::from_slice(&std::fs::read(report_path).expect("fresh report"))
        .expect("decode report");
    (output, report)
}

#[test]
fn recursive_discovery_and_census_share_sources_without_running_dependency_tests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("create project");
    write(temp.path(), ".gitignore", "*\n");
    git(&root, &["init", "-q"]);
    write(&root, ".gitignore", "ignored/\n");
    write(&root, ".agentignore", "agent_ignored/\n");
    for (path, name) in [
        ("tests/test_visible.harn", "test_visible"),
        (".github/test_hidden_source.harn", "test_hidden_source"),
        ("vendor/test_vendor.harn", "test_vendor"),
    ] {
        write(
            &root,
            path,
            &format!("pipeline {name}() {{ assert(true) }}\n"),
        );
    }
    write(
        &root,
        "lib/helper.harn",
        "pub fn value() -> int { return 1 }\n",
    );
    for target in ["source_without_extension", "explicit.harn.txt"] {
        write(
            &root,
            target,
            "pipeline test_explicit_source() { assert(true) }\n",
        );
    }
    for path in [
        ".harn/vendor/pkg/test_poison.harn",
        "ignored/test_poison.harn",
        "agent_ignored/test_poison.harn",
        "target/test_poison.harn",
    ] {
        write(
            &root,
            path,
            "pipeline test_poison() { assert(false, \"dependency poison reached\") }\n",
        );
    }
    let (output, report) = run(&root, ".");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report["summary"]["total"], 3);
    assert_eq!(report["summary"]["passed"], 3);
    assert_eq!(report["summary"]["failed"], 0);
    let names = report["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| case["name"].as_str().expect("case name"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        std::collections::BTreeSet::from(["test_visible", "test_hidden_source", "test_vendor",])
    );
    assert!(
        stdout.contains("ran 3 case(s) from 4 discoverable source file(s)"),
        "{stdout}"
    );

    for target in [
        ".harn/vendor/pkg/test_poison.harn",
        ".harn/vendor/pkg",
        "ignored",
    ] {
        let (output, report) = run(&root, target);
        assert_eq!(output.status.code(), Some(harn_cli::exit::PROGRAM_FAILURE));
        assert_eq!(report["summary"]["total"], 1);
        assert_eq!(report["summary"]["failed"], 1);
        assert_eq!(report["cases"][0]["name"], "test_poison");
        assert!(report["cases"][0]["message"]
            .as_str()
            .expect("failure")
            .contains("dependency poison reached"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("ran 1 case(s) from 1 discoverable source file(s)"),
            "{stdout}"
        );
    }

    for target in ["source_without_extension", "explicit.harn.txt"] {
        let (output, report) = run(&root, target);
        assert!(output.status.success());
        assert_eq!(report["summary"]["total"], 1);
        assert_eq!(report["summary"]["passed"], 1);
        assert_eq!(report["summary"]["skipped"], 0);
        assert_eq!(report["cases"][0]["name"], "test_explicit_source");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("ran 1 case(s) from 1 discoverable source file(s)"),
            "{stdout}"
        );
    }
}
