//! Product-path smoke tests for `harn serve mcp`.
//!
//! Protocol shape and handler catalogs are covered in-process. These tests
//! retain only what the binary surface uniquely owns: real stdio framing and
//! stable stateless HTTP multi-round-trip re-entry.

use crate::test_util;

use std::fs;
use std::process::Stdio;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;
use test_util::stdio_jsonrpc::StdioJsonRpcClient;

const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);
const PROTOCOL_VERSION: &str = "2026-07-28";

fn stable_meta(capabilities: JsonValue) -> JsonValue {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "harn-e2e-client",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    })
}

fn stable_request(id: u64, method: &str, mut params: JsonValue) -> JsonValue {
    params["_meta"] = stable_meta(json!({
        "elicitation": {"form": {}, "url": {}},
        "roots": {},
        "sampling": {},
    }));
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn write_export_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
pub fn greet(name: string, excited: bool = false) -> dict {
  if excited {
    return {message: "Hello, " + name + "!"}
  }
  return {message: "Hello, " + name}
}
"#,
    )
    .unwrap();
}

fn write_registry_info_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
import { tool_registry_from } from "std/tools"

fn main(harness: Harness) {
  const tools = tool_registry_from([
    {
      name: "greet",
      description: "Greet one person.",
      parameters: {name: {schema: {type: "string"}, required: true}},
      handler: {args -> {message: "Hello, " + args.name}},
    },
  ], {name: "widgets", version: "1.2.3", description: "Widget integration"})
  harness.tools.mcp_tools(tools)
}
"#,
    )
    .unwrap();
}

fn write_reload_registry_fixture(temp: &TempDir, tool_name: &str, value: &str) {
    fs::write(
        temp.path().join("server.harn"),
        format!(
            r#"
import {{ tool_registry_from }} from "std/tools"

fn main(harness: Harness) {{
  const tools = tool_registry_from([
    {{
      name: "{tool_name}",
      description: "Return the active fixture value.",
      parameters: {{}},
      returns: {{
        type: "object",
        properties: {{value: {{type: "string"}}}},
        required: ["value"],
        additionalProperties: false,
      }},
      handler: {{_args -> {{value: "{value}"}}}},
    }},
  ], {{name: "reload-fixture"}})
  harness.tools.mcp_tools(tools)
}}
"#
        ),
    )
    .unwrap();
}

fn write_authority_export_fixture(temp: &TempDir) {
    fs::create_dir_all(temp.path().join("nested")).unwrap();
    fs::write(
        temp.path().join("nested/server.harn"),
        r"
pub fn inspect(harness: Harness, hypothesis_id: string) -> dict {
  return {hypothesis_id: hypothesis_id, has_root: harness != nil}
}
",
    )
    .unwrap();
}

fn write_elicitation_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
fn main(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "ask", "Ask for deployment input", {
    parameters: {prompt: "string"},
    handler: { args ->
      const response = harness.tools.mcp_elicit({
        message: args.prompt,
        requestedSchema: {
          type: "object",
          properties: {env: {type: "string"}, confirm: {type: "boolean"}},
          required: ["env", "confirm"],
        },
      })
      const content = response.content ?? {}
      return to_string(response.action) + ":" + to_string(content.env ?? "") + ":"
        + to_string(content.confirm ?? "")
    },
  })
  harness.tools.mcp_tools(tools)
}
"#,
    )
    .unwrap();
}

fn write_mixed_surface_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
pub fn render_value(args: dict) {
  return {rendered: args.value}
}

pipeline default(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "render_fixture", "Render one fixture value", {
    parameters: {
      type: "object",
      properties: {value: {type: "string"}},
      required: ["value"],
    },
    handler: render_value,
  })
  harness.tools.mcp_tools(tools)
}
"#,
    )
    .unwrap();
}

fn write_trusted_host_dispatch_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("harn.toml"),
        "[check]\ntrusted_host_dispatch = true\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("server.harn"),
        r#"
pub fn host_environment() -> dict {
  return host_call("env.host", {})
}
"#,
    )
    .unwrap();
}

fn wait_for_http_listener(child: &mut std::process::Child, rx: &Receiver<String>) -> String {
    test_util::stdio_jsonrpc::wait_for_child_log_suffix(
        child,
        rx,
        "MCP workflow server ready on ",
        PROCESS_READY_TIMEOUT,
        "HTTP MCP server",
    )
}

#[ignore = "binary surface: runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_stdio_discovers_and_calls_exported_tool() {
    let temp = TempDir::new().unwrap();
    write_export_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);

    let discovery = client.request(stable_request(1, "server/discover", json!({})));
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert_eq!(
        discovery["result"]["supportedVersions"],
        json!([PROTOCOL_VERSION])
    );

    let tools = client.request(stable_request(2, "tools/list", json!({})));
    assert_eq!(tools["result"]["tools"][0]["name"], "greet");

    let called = client.request(stable_request(
        3,
        "tools/call",
        json!({"name": "greet", "arguments": {"name": "Harn", "excited": true}}),
    ));
    assert_eq!(called["result"]["resultType"], "complete");
    assert_eq!(
        called["result"]["structuredContent"]["message"],
        "Hello, Harn!"
    );
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_script_surface_wins_over_public_helpers_when_explicit() {
    let temp = TempDir::new().unwrap();
    write_mixed_surface_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--surface")
        .arg("script")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp --surface script", command);

    let initialized = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "released-client", "version": "1"}
        }
    }));
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = client.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["render_fixture"]);

    let called = client.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "render_fixture", "arguments": {"value": "crystallized"}}
    }));
    let content = called["result"]["content"][0]["text"]
        .as_str()
        .expect("script tool text result");
    assert!(
        content.contains("crystallized"),
        "unexpected result: {content}"
    );
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_injects_authority_for_relative_nested_script() {
    let temp = TempDir::new().unwrap();
    write_authority_export_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("nested/server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);

    let _ = client.request(stable_request(1, "server/discover", json!({})));
    let tools = client.request(stable_request(2, "tools/list", json!({})));
    let inspect = &tools["result"]["tools"][0];
    assert_eq!(inspect["name"], "inspect");
    assert!(inspect["inputSchema"]["properties"]
        .get("harness")
        .is_none());
    assert_eq!(inspect["inputSchema"]["required"], json!(["hypothesis_id"]));

    let called = client.request(stable_request(
        3,
        "tools/call",
        json!({"name": "inspect", "arguments": {"hypothesis_id": "hypothesis-1"}}),
    ));
    assert_eq!(
        called["result"]["structuredContent"]["hypothesis_id"],
        "hypothesis-1"
    );
    assert_eq!(called["result"]["structuredContent"]["has_root"], true);
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_stdio_initializes_and_calls_from_released_client() {
    let temp = TempDir::new().unwrap();
    write_export_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);

    let initialized = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "codex-mcp-client", "version": "test"}
        }
    }));
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "server");

    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    let tools = client.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    assert_eq!(tools["result"]["tools"][0]["name"], "greet");
    assert!(tools["result"].get("resultType").is_none());

    let called = client.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "greet",
            "arguments": {"name": "Codex", "excited": true},
            "_meta": {"progressToken": "codex-proof"}
        }
    }));
    assert_eq!(
        called["result"]["structuredContent"]["message"],
        "Hello, Codex!"
    );
    assert!(called["result"].get("resultType").is_none());
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_uses_registry_identity_when_transport_metadata_is_absent() {
    let temp = TempDir::new().unwrap();
    write_registry_info_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);

    let initialized = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "codex-mcp-client", "version": "test"}
        }
    }));
    assert_eq!(initialized["result"]["serverInfo"]["name"], "widgets");
    assert_eq!(initialized["result"]["serverInfo"]["version"], "1.2.3");
    assert_eq!(initialized["result"]["instructions"], "Widget integration");
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_watch_keeps_one_client_across_valid_and_invalid_registry_edits() {
    let temp = TempDir::new().unwrap();
    write_reload_registry_fixture(&temp, "before_reload", "v1");
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--surface")
        .arg("script")
        .arg("--watch")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp --watch", command);

    let initialized = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "reload-client", "version": "1"}
        }
    }));
    assert_eq!(
        initialized["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    let before = client.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "before_reload", "arguments": {}}
    }));
    assert_eq!(before["result"]["structuredContent"]["value"], "v1");

    fs::write(temp.path().join("server.harn"), "fn main(").unwrap();
    client.wait_for_stderr("reload failed; keeping previous registry");
    let after_rejected_reload = client.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "before_reload", "arguments": {}}
    }));
    assert_eq!(
        after_rejected_reload["result"]["structuredContent"]["value"],
        "v1"
    );

    write_reload_registry_fixture(&temp, "after_reload", "v2");
    let notification = client.recv_until(
        |_| {},
        |message| message["method"] == "notifications/tools/list_changed",
    );
    assert_eq!(notification["params"], json!({}));
    for method in [
        "notifications/resources/list_changed",
        "notifications/prompts/list_changed",
    ] {
        let notification = client.recv_until(|_| {}, |message| message["method"] == method);
        assert_eq!(notification["params"], json!({}));
    }

    let tools = client.request(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/list",
        "params": {}
    }));
    assert_eq!(tools["result"]["tools"][0]["name"], "after_reload");
    let after = client.request(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {"name": "after_reload", "arguments": {}}
    }));
    assert_eq!(after["result"]["structuredContent"]["value"], "v2");
    client.shutdown_expect_success();
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_honors_manifest_trusted_host_dispatch() {
    let temp = TempDir::new().unwrap();
    write_trusted_host_dispatch_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn")
        .env_remove("HARN_LEGACY_AMBIENT_CAPABILITIES");
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);

    let _ = client.request(stable_request(1, "server/discover", json!({})));
    let called = client.request(stable_request(
        2,
        "tools/call",
        json!({"name": "host_environment", "arguments": {}}),
    ));
    let content = called["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text");
    assert!(
        content.contains("unsupported operation env.host"),
        "manifest authority should reach runtime dispatch: {content}"
    );
    assert!(
        !content.contains("not callable source API"),
        "manifest authority was ignored: {content}"
    );
    client.shutdown_expect_success();
}

async fn post_stable(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    name: Option<&str>,
    body: &JsonValue,
) -> reqwest::Response {
    let mut request = client
        .post(url)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .header("Mcp-Method", method);
    if let Some(name) = name {
        request = request.header("Mcp-Name", name);
    }
    request.json(body).send().await.unwrap()
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[tokio::test]
async fn serve_mcp_http_reenters_handler_with_stable_input_response() {
    let temp = TempDir::new().unwrap();
    write_elicitation_fixture(&temp);
    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("--transport")
        .arg("http")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("server.harn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (rx, stderr) = test_util::stdio_jsonrpc::spawn_line_reader(child.stderr.take().unwrap());
    let url = wait_for_http_listener(&mut child, &rx);
    let client = reqwest::Client::new();

    let first = stable_request(
        1,
        "tools/call",
        json!({"name": "ask", "arguments": {"prompt": "Choose deploy target"}}),
    );
    let first_response = post_stable(&client, &url, "tools/call", Some("ask"), &first).await;
    assert!(first_response.headers().get("mcp-session-id").is_none());
    let first_body: JsonValue = first_response.json().await.unwrap();
    let required = &first_body["result"];
    assert_eq!(required["resultType"], "input_required");
    let (input_key, input_request) = required["inputRequests"]
        .as_object()
        .and_then(|requests| requests.iter().next())
        .expect("one embedded input request");
    assert_eq!(input_request["method"], "elicitation/create");
    assert_eq!(input_request["params"]["mode"], "form");

    let retry = stable_request(
        2,
        "tools/call",
        json!({
            "name": "ask",
            "arguments": {"prompt": "Choose deploy target"},
            "requestState": required["requestState"].clone(),
            "inputResponses": {
                (input_key): {"action": "accept", "content": {"env": "staging", "confirm": true}}
            }
        }),
    );
    let retry_body: JsonValue = post_stable(&client, &url, "tools/call", Some("ask"), &retry)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(retry_body["result"]["resultType"], "complete");
    assert_eq!(
        retry_body["result"]["content"][0]["text"],
        "accept:staging:true"
    );

    child.kill().unwrap();
    child.wait().unwrap();
    stderr.join().unwrap();
}
