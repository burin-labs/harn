//! End-to-end coverage for `harn graph --json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn run_harn(args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(args)
        .output()
        .expect("spawn harn")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

fn write_fixture(root: &Path) {
    fs::write(
        root.join("main.harn"),
        r#"
import { format_title } from "util"

pub fn main() -> string {
  let title = format_title("Graph")
  let body = read_file("README.md")
  return title + body
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("util.harn"),
        r#"
import { Thing } from "types"

pub fn format_title(value: string) -> string {
  let prompt = "Title: " + value
  let _response = llm_call(prompt, {provider: "auto"})
  return value
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("types.harn"),
        r#"
type Thing = {name: string}
"#,
    )
    .unwrap();
}

#[test]
fn graph_json_reports_modules_symbols_capabilities_and_edges() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());

    let root = temp.path().to_str().unwrap();
    let output = run_harn(&["graph", root, "--json"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);

    let modules = parsed["data"]["modules"].as_array().unwrap();
    assert_eq!(modules.len(), 3);
    assert_eq!(
        modules
            .iter()
            .map(|module| module["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["main.harn", "types.harn", "util.harn"]
    );

    let main = modules
        .iter()
        .find(|module| module["path"] == "main.harn")
        .unwrap();
    assert_eq!(main["imports"], serde_json::json!(["util.harn"]));
    assert_eq!(
        main["requires_capabilities"],
        serde_json::json!(["workspace.read_text"])
    );
    assert_eq!(main["effects"], serde_json::json!(["fs.read"]));
    assert_eq!(main["host_calls"], serde_json::json!(["read_file"]));
    assert_eq!(main["public_symbols"][0]["name"], "main");
    assert_eq!(main["public_symbols"][0]["kind"], "fn");
    assert_eq!(
        main["public_symbols"][0]["signature"],
        "fn main() -> string"
    );

    let util = modules
        .iter()
        .find(|module| module["path"] == "util.harn")
        .unwrap();
    assert_eq!(util["imports"], serde_json::json!(["types.harn"]));
    assert_eq!(
        util["requires_capabilities"],
        serde_json::json!(["llm.call"])
    );
    assert_eq!(util["effects"], serde_json::json!(["llm.call"]));
    assert_eq!(util["host_calls"], serde_json::json!(["llm_call"]));

    assert_eq!(
        parsed["data"]["graph"]["nodes"],
        serde_json::json!(["main.harn", "types.harn", "util.harn"])
    );
    assert_eq!(
        parsed["data"]["graph"]["edges"],
        serde_json::json!([
            {"from": "main.harn", "to": "util.harn"},
            {"from": "util.harn", "to": "types.harn"}
        ])
    );
}

#[test]
fn graph_json_module_filter_focuses_modules_but_keeps_edge_targets() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());

    let root = temp.path().to_str().unwrap();
    let output = run_harn(&["graph", root, "--json", "--module", "util"]);
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = stdout_json(&output);
    let modules = parsed["data"]["modules"].as_array().unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["path"], "util.harn");
    assert_eq!(
        parsed["data"]["graph"]["nodes"],
        serde_json::json!(["types.harn", "util.harn"])
    );
}

#[test]
fn graph_json_surfaces_declared_stdlib_metadata_per_symbol() {
    let temp = TempDir::new().unwrap();
    let module = temp.path().join("annotated.harn");
    fs::write(
        &module,
        r#"
/**
 * Read a file and return its contents.
 *
 * @effects: [fs.read]
 * @allocation: heap
 * @errors: [FileNotFound]
 * @api_stability: stable
 * @example: read_file("README.md")
 */
pub fn read_file(path: string) -> string {
  return path
}
"#,
    )
    .unwrap();

    let root = temp.path().to_str().unwrap();
    let output = run_harn(&["graph", root, "--json"]);
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let parsed = stdout_json(&output);

    let symbols = parsed["data"]["modules"][0]["public_symbols"]
        .as_array()
        .expect("public_symbols array");
    let read_file_sym = symbols
        .iter()
        .find(|s| s["name"] == "read_file")
        .expect("read_file symbol");
    let meta = &read_file_sym["metadata"];
    assert_eq!(meta["effects"], serde_json::json!(["fs.read"]));
    assert_eq!(meta["allocation"], "heap");
    assert_eq!(meta["errors"], serde_json::json!(["FileNotFound"]));
    assert_eq!(meta["api_stability"], "stable");
    assert_eq!(meta["example"], "read_file(\"README.md\")");
}

#[test]
fn graph_json_attributes_harness_sub_handle_calls_to_capabilities() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("main.harn"),
        r#"
fn main(harness: Harness) {
  let body = harness.fs.read_text("README.md")
  harness.net.get("https://example.test/data")
  harness.stdio.println(body)
}
"#,
    )
    .unwrap();

    let root = temp.path().to_str().unwrap();
    let output = run_harn(&["graph", root, "--json"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    let modules = parsed["data"]["modules"].as_array().unwrap();
    let main = modules
        .iter()
        .find(|module| module["path"] == "main.harn")
        .expect("main.harn module");
    let caps = main["requires_capabilities"]
        .as_array()
        .expect("requires_capabilities array");
    let cap_strings: Vec<&str> = caps.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        cap_strings.contains(&"workspace.read_text"),
        "expected harness.fs.read_text to produce workspace.read_text capability, got: {cap_strings:?}"
    );
    assert!(
        cap_strings.contains(&"network.http"),
        "expected harness.net.get to produce network.http capability, got: {cap_strings:?}"
    );
}

#[test]
fn graph_appears_in_json_schemas_catalog() {
    let output = run_harn(&["--json-schemas", "--command", "graph"]);
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);
    let entries = parsed["data"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["command"], "graph");
    assert_eq!(entries[0]["schemaVersion"], 1);
}
