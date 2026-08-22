use std::fs;
use std::process::{Command, Output};

use crate::test_util::process::harn_e2e_binary;

fn run_script(script: &std::path::Path) -> Output {
    Command::new(harn_e2e_binary())
        .args(["run", script.to_str().expect("UTF-8 script path")])
        .output()
        .expect("run Harn script")
}

#[test]
fn path_metadata_set_is_visible_to_the_next_cli_process() {
    let project = tempfile::tempdir().expect("temp project");
    let set_script = project.path().join("set.harn");
    let get_script = project.path().join("get.harn");

    fs::write(
        &set_script,
        r#"fn main(harness: Harness) {
  harness.project.path_metadata_set({
    path: "notes.txt",
    namespace: "probe:ns",
    data: {probe: true},
    options: {},
  })
  harness.project.path_metadata_set({
    path: "docs",
    namespace: "probe:ns",
    data: {owner: "team"},
    options: {kind: "dir"},
  })
  harness.stdio.print("SET_RETURNED")
}
"#,
    )
    .expect("write set script");
    fs::write(
        &get_script,
        r#"fn main(harness: Harness) {
  const record = harness.project.path_metadata_get({
    path: "notes.txt",
    namespace: "probe:ns",
    options: {},
  })
  const entries = harness.project.path_metadata_entries({
    namespace: "probe:ns",
    options: {kind: "all"},
  })
  const directory = harness.project.path_metadata_get({
    path: "docs",
    namespace: "probe:ns",
    options: {kind: "dir"},
  })
  harness.stdio.print("RECORD=" + json_stringify(record))
  harness.stdio.print("DIRECTORY=" + json_stringify(directory))
  harness.stdio.print("ENTRIES=" + json_stringify(entries))
}
"#,
    )
    .expect("write get script");

    let set = run_script(&set_script);
    assert!(
        set.status.success(),
        "set failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&set.stdout), "SET_RETURNED");

    let shard = project
        .path()
        .join(".harn/metadata/probe_3Ans/entries.json");
    assert!(
        shard.is_file(),
        "path metadata was not durable when set returned: {}",
        shard.display()
    );

    let get = run_script(&get_script);
    assert!(
        get.status.success(),
        "get failed: {}",
        String::from_utf8_lossy(&get.stderr)
    );
    let stdout = String::from_utf8_lossy(&get.stdout);
    assert!(stdout.contains(r#"RECORD={"probe":true}"#), "{stdout}");
    assert!(stdout.contains(r#"DIRECTORY={"owner":"team"}"#), "{stdout}");
    assert!(
        stdout.contains(r#"{"kind":"file","local":{"probe":true},"path":"notes.txt"}"#),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            r#"{"kind":"dir","local":{"owner":"team"},"path":"docs","resolved":{"owner":"team"}}"#
        ),
        "{stdout}"
    );
}
