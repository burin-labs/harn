use std::path::Path;

use crate::test_util::process::harn_e2e_command;

fn run(root: &Path, cache: &Path) -> serde_json::Value {
    let home = tempfile::tempdir().unwrap();
    let output = harn_e2e_command()
        .args(["time", "run", "--json"])
        .arg(root.join("main.harn"))
        .env("HARN_CACHE_DIR", cache)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("shipped-interface-reached"),
        "the imported enum-dependent function must execute: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("time run receipt")
}

fn entries(cache: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn relocated_precompiled_tree_writes_no_shared_cache_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let build = tmp.path().join("build");
    let package = tmp.path().join("release-package");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(
        build.join("leaf.harn"),
        "pub enum Color { Ready(message: string) Empty }\n",
    )
    .unwrap();
    std::fs::write(
        build.join("lib.harn"),
        r#"import "./leaf"
pub fn answer() -> string {
  match Color.Ready("shipped-interface-reached") {
    Color.Ready(message) -> { return message }
    _ -> { return "wrong" }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        build.join("main.harn"),
        r#"import { answer } from "./lib"
fn main(harness: Harness) {
  harness.llm.mock_load_jsonl(json_stringify({text: answer(), model: "first-turn-fixture"}) + "\n")
  const reply = harness.llm.call("Say hello.", nil, {provider: "mock", model: "first-turn-fixture"})
  harness.stdio.println(reply.text)
}
"#,
    )
    .unwrap();
    let output = harn_e2e_command()
        .arg("precompile")
        .arg(&build)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["main", "lib", "leaf"] {
        for extension in ["harnbc", "harnmod", "harnifc"] {
            assert!(build.join(format!("{name}.{extension}")).is_file());
        }
    }
    std::fs::rename(&build, &package).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let receipt = run(&package, cache.path());
    let compile = receipt["data"]["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["name"] == "bytecode_compile")
        .expect("compile phase reported");
    assert_eq!(compile["cache"], "hit", "{receipt}");
    assert!(
        entries(cache.path()).is_empty(),
        "cold shared cache received {:?}",
        entries(cache.path())
    );

    // Removing every interface prevents entry linking from supplying the
    // missing module's projection. A real runtime derivation must be visible.
    for name in ["main", "lib", "leaf"] {
        std::fs::remove_file(package.join(format!("{name}.harnifc"))).unwrap();
    }
    let missing_cache = tempfile::tempdir().unwrap();
    run(&package, missing_cache.path());
    assert!(
        entries(missing_cache.path())
            .iter()
            .any(|name| name.ends_with(".harnifc")),
        "missing interfaces must rederive into the shared cache: {:?}",
        entries(missing_cache.path())
    );
}
