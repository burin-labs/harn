mod test_util;

use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;
use test_util::stdio_jsonrpc::StdioJsonRpcClient;

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn spawn_worker() -> StdioJsonRpcClient {
    let mut command = harn_e2e_command();
    command.args(["serve", "test"]);
    StdioJsonRpcClient::spawn("harn serve test", command)
}

/// The worker is strict request/response: one JSON line back per request, no
/// interleaved notifications. `recv` reads exactly that response, bounded by
/// the client deadline instead of the old unbounded `read_line` (harn#5401).
fn request(client: &mut StdioJsonRpcClient, value: Value) -> Value {
    client.send(&value);
    client.recv()
}

fn initialize_worker(client: &mut StdioJsonRpcClient) -> Value {
    request(
        client,
        json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {"protocol_version": "1"},
        }),
    )
}

fn run_suite(id: i64, path: &Path, client: &mut StdioJsonRpcClient) -> Value {
    request(
        client,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "test/run",
            "params": {"path": path, "timeout_ms": 5_000},
        }),
    )
}

fn shutdown_worker(client: &mut StdioJsonRpcClient) -> Value {
    request(
        client,
        json!({"jsonrpc": "2.0", "id": "stop", "method": "shutdown"}),
    )
}

fn write_counter_suite(temp: &TempDir, initial: i64) -> std::path::PathBuf {
    write_file(
        temp.path(),
        "suite/counter.harn",
        &format!(
            r"
let count = {initial}

pub fn increment() {{
  count = count + 1
  return count
}}
"
        ),
    );
    write_file(
        temp.path(),
        "suite/test_counter.harn",
        &format!(
            r#"
import {{ increment }} from "./counter"

pipeline test_counter(_task) {{
  assert_eq(increment(), {})
  assert_eq(increment(), {})
}}
"#,
            initial + 1,
            initial + 2,
        ),
    );
    temp.path().join("suite/test_counter.harn")
}

#[test]
fn stdio_worker_reuses_prepared_modules_without_leaking_state() {
    let temp = TempDir::new().unwrap();
    let suite = write_counter_suite(&temp, 0);
    let mut client = spawn_worker();

    let initialized = initialize_worker(&mut client);
    assert_eq!(initialized["result"]["protocol_version"], "1");
    assert!(initialized["result"]["server_version"].is_string());
    assert_eq!(
        initialized["result"]["capabilities"]["test_run"]["schema_version"],
        3
    );

    let first = run_suite(1, &suite, &mut client);
    let second = run_suite(2, &suite, &mut client);

    assert_eq!(
        first["result"]["worker_id"],
        initialized["result"]["worker_id"]
    );
    assert_eq!(
        second["result"]["process_id"],
        initialized["result"]["process_id"]
    );
    assert_eq!(first["result"]["summary"]["passed"], 1);
    assert_eq!(second["result"]["summary"]["passed"], 1);
    assert_eq!(second["result"]["run_count"], 2);
    assert_eq!(second["result"]["schema_version"], 3);
    assert_eq!(
        second["result"]["summary"]["results"][0]["phases"]["modules"]["modules_compiled"],
        0
    );
    assert_eq!(
        second["result"]["summary"]["results"][0]["phases"]["modules"]["modules_loaded"],
        1
    );
    assert!(
        second["result"]["cache_after"]["hits"].as_u64().unwrap()
            > second["result"]["cache_before"]["hits"].as_u64().unwrap()
    );
    assert_eq!(
        second["result"]["cache_after"]["insertions"],
        second["result"]["cache_before"]["insertions"]
    );

    write_counter_suite(&temp, 40);
    let after_edit = run_suite(3, &suite, &mut client);
    assert_eq!(after_edit["result"]["summary"]["passed"], 1);
    assert!(
        after_edit["result"]["cache_after"]["insertions"]
            .as_u64()
            .unwrap()
            > after_edit["result"]["cache_before"]["insertions"]
                .as_u64()
                .unwrap()
    );

    let shutdown = shutdown_worker(&mut client);
    assert_eq!(shutdown["result"]["run_count"], 3);
    assert_eq!(
        shutdown["result"]["worker_id"],
        initialized["result"]["worker_id"]
    );
    client.shutdown_expect_success();
}

#[test]
fn stdio_worker_survives_invalid_project_configuration() {
    let temp = TempDir::new().unwrap();
    let suite = write_counter_suite(&temp, 0);
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"

[[handoff_routes]]
kind = "merge_receipt"
from = "merge_captain"
route = [{ target = "missing_persona", when = "always" }]
"#,
    );
    let mut client = spawn_worker();

    let initialized = initialize_worker(&mut client);
    assert_eq!(initialized["result"]["protocol_version"], "1");

    let invalid = run_suite(1, &suite, &mut client);
    assert_eq!(invalid["result"]["summary"]["failed"], 1);
    assert!(invalid["result"]["summary"]["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("failed to load runtime extensions"));

    fs::remove_file(temp.path().join("harn.toml")).unwrap();
    let recovered = run_suite(2, &suite, &mut client);
    assert_eq!(recovered["result"]["summary"]["passed"], 1);

    let shutdown = shutdown_worker(&mut client);
    assert_eq!(shutdown["result"]["run_count"], 2);
    assert_eq!(
        shutdown["result"]["worker_id"],
        initialized["result"]["worker_id"]
    );
    client.shutdown_expect_success();
}

#[test]
fn stdio_worker_keeps_protocol_stdin_out_of_user_tests() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "test_stdin.harn",
        r#"
import { configure, log } from "std/observability"

pipeline test_stdin(_task) {
  assert_eq(read_stdin(), nil)
  assert_eq(read_stdin(), nil)
  assert_eq(harness.stdio.read_line(), nil)
  assert_eq(harness.stdio.prompt("ignored: "), nil)
  assert_eq(host_call("interaction.ask", {question: "ignored: "}), nil)
  configure({backend: {kind: "pretty_stdout", id: "pretty_stdout"}})
  log("must-not-corrupt-jsonrpc")
}
"#,
    );
    let suite = temp.path().join("test_stdin.harn");
    let mut client = spawn_worker();

    initialize_worker(&mut client);
    let result = run_suite(1, &suite, &mut client);
    assert_eq!(result["result"]["summary"]["passed"], 1);
    assert_eq!(shutdown_worker(&mut client)["result"]["run_count"], 1);

    client.shutdown_expect_success();
}

#[test]
fn stdio_worker_drops_removed_manifest_mock_declarations() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        "[check]\nhost_capabilities.synthetic_fixture = [\"answer\"]\n",
    );
    write_file(
        temp.path(),
        "test_manifest_mock.harn",
        r#"
import { with_host_mocks } from "std/testing"

pipeline test_manifest_mock(_task) {
  with_host_mocks(
    [{capability: "synthetic_fixture", operation: "answer", result: 42}],
    { _ -> assert_eq(host_call("synthetic_fixture.answer", {}), 42) },
  )
}
"#,
    );
    let suite = temp.path().join("test_manifest_mock.harn");
    let mut client = spawn_worker();

    initialize_worker(&mut client);
    let declared = run_suite(1, &suite, &mut client);
    assert_eq!(declared["result"]["summary"]["passed"], 1);

    write_file(temp.path(), "harn.toml", "[check]\n");
    let removed = run_suite(2, &suite, &mut client);
    assert_eq!(removed["result"]["summary"]["failed"], 1);
    assert!(removed["result"]["summary"]["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("unregistered host operation"));
    assert_eq!(shutdown_worker(&mut client)["result"]["run_count"], 2);

    client.shutdown_expect_success();
}
