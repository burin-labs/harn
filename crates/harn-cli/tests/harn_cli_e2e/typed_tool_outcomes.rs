use std::fs;

use serde_json::{json, Value};

use crate::test_util::process::harn_e2e_command;
use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;

const FIXTURE: &str = r#"
import { tool_registry_from } from "std/tools"
import { agent_dispatch_tool_call } from "std/agent/primitives"
import { agent_lifecycle_tools } from "std/agent/workers"
import { AgentToolHandlerResult, agent_tool_handler_result } from "std/agent/tool_lifecycle"

struct DomainPayload {
  ok: bool
  success: bool
  status: string
}

type WidgetFailure = {code: string}
type Widget = {label: string}
type DomainEnvelope = {
  schema: "ordinary.domain.v2",
  outcome: "ok" | "error" | "rejected",
  text: string,
  data: Widget,
}

fn record_call(harness: Harness, name: string) {
  const path = name + ".calls"
  const previous = if harness.fs.exists(path) { harness.fs.read_text(path) } else { "" }
  harness.fs.write_text(path, previous + "called\n")
}

fn application_failure(harness: Harness) -> any throws WidgetFailure {
  record_call(harness, "application")
  throw {code: "conflict"}
}

fn export_result(mode: string) -> any {
  if mode == "malformed" {
    return {schema: "harn.agent_tool_handler_result.v2", outcome: "ok", data: {label: "value"}}
  }
  return agent_tool_handler_result(
    "Export feedback for " + mode,
    if mode == "invalid" { {wrong: true} } else { {label: "value"} },
    if mode == "error" { "error" } else if mode == "rejected" { "rejected" } else { "ok" },
  )
}

pub fn exported_payload(harness: Harness, mode: string) -> AgentToolHandlerResult<Widget> {
  record_call(harness, "export_" + mode)
  return export_result(mode)
}

pub fn exported_nominal(harness: Harness) -> DomainPayload {
  record_call(harness, "export_nominal")
  return DomainPayload {ok: false, success: false, status: "error"}
}

pub fn exported_domain(harness: Harness) -> DomainEnvelope {
  record_call(harness, "export_domain")
  return {schema: "ordinary.domain.v2", outcome: "error", text: "Domain text", data: {label: "value"}}
}

fn registry(harness: Harness) {
  const payload_schema = {
    type: "object", properties: {label: {type: "string"}},
    required: ["label"], additionalProperties: false,
  }
  let specs = []
  for name in ["ok", "invalid", "error", "rejected", "malformed"] {
    specs = specs + [{
      name: name, description: "Exercise one explicit tool result.",
      parameters: {}, returns: payload_schema,
      handler: { _ ->
        record_call(harness, name)
        if name == "malformed" {
          {schema: "harn.agent_tool_handler_result.v2", outcome: "ok", data: {label: "value"}}
        } else {
          agent_tool_handler_result(
            "Feedback for " + name,
            if name == "invalid" { {wrong: true} } else { {label: "value"} },
            if name == "error" { "error" } else if name == "rejected" { "rejected" } else { "ok" },
          )
        }
      },
    }]
  }
  for name in ["raw", "nominal"] {
    specs = specs + [{
      name: name, description: "Ordinary domain payloads preserve their meaning.", parameters: {},
      returns: {
        type: "object", properties: {
          ok: {const: false}, success: {const: false}, status: {const: "error"},
        }, required: ["ok", "success", "status"], additionalProperties: false,
      },
      handler: { _ ->
        record_call(harness, name)
        if name == "nominal" {
          DomainPayload {ok: false, success: false, status: "error"}
        } else { {ok: false, success: false, status: "error"} }
      },
    }]
  }
  return tool_registry_from(specs + [{
    name: "application", description: "Preserve the declared application error channel.",
    parameters: {}, returns: payload_schema,
    error_schema: {
      type: "object", properties: {code: {const: "conflict"}},
      required: ["code"], additionalProperties: false,
    },
    handler: { _ -> application_failure(harness) },
  }])
}

fn main(harness: Harness) {
  harness.tools.mcp_tools(registry(harness))
}

@test
pipeline agent_payload_contract(harness: Harness) {
  const direct = registry(harness)
  const owned = agent_lifecycle_tools(harness.agent, direct)
  for tools in [direct, owned] {
    for name in ["ok", "invalid", "error", "rejected", "malformed"] {
      const result = agent_dispatch_tool_call(harness.tools, {name: name, arguments: {}}, tools)
      if name == "ok" {
        assert(result.ok)
        assert(result.result.schema == "harn.agent_tool_handler_result.v2")
        assert(result.result.data == {label: "value"})
        assert(result.rendered_result == "Feedback for ok")
      } else {
        assert(!result.ok, name + " must fail before agent success")
        assert(result.error_category == if name == "error" {
          "tool_error"
        } else if name == "rejected" { "tool_rejected" } else { "schema_validation" })
      }
    }
  }
}
"#;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temporary product fixture");
    fs::write(temp.path().join("tools.harn"), FIXTURE).expect("write tool fixture");
    temp
}

fn calls(temp: &tempfile::TempDir, name: &str) -> usize {
    fs::read_to_string(temp.path().join(format!("{name}.calls")))
        .expect("the actual handler must record its invocation")
        .lines()
        .count()
}

#[test]
fn agent_validates_payload_without_losing_the_explicit_outcome() {
    let temp = fixture();
    let result = harn_e2e_command()
        .current_dir(temp.path())
        .args(["test", "tools.harn"])
        .output()
        .expect("run actual agent dispatch");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("1 passed"), "no agent case ran: {stdout}");
    for name in ["ok", "invalid", "error", "rejected", "malformed"] {
        assert_eq!(
            calls(&temp, name),
            2,
            "direct and lifecycle-owned handler: {name}"
        );
    }
}

fn request(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "typed-outcome-proof", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[test]
fn cli_and_mcp_project_validated_payload_and_canonical_feedback() {
    let temp = fixture();
    let domain = json!({"ok": false, "success": false, "status": "error"});
    for name in [
        "ok",
        "invalid",
        "error",
        "rejected",
        "malformed",
        "raw",
        "nominal",
        "application",
    ] {
        let result = harn_e2e_command()
            .current_dir(temp.path())
            .args([
                "tool",
                "run",
                "tools.harn",
                name,
                "--harn-input",
                "{}",
                "--json",
            ])
            .output()
            .expect("run actual generated CLI");
        let succeeds = matches!(name, "ok" | "raw" | "nominal");
        assert_eq!(
            result.status.success(),
            succeeds,
            "CLI {name}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if succeeds {
            let payload: Value = serde_json::from_slice(&result.stdout).expect("CLI API payload");
            assert_eq!(
                payload,
                if name == "ok" {
                    json!({"label": "value"})
                } else {
                    domain.clone()
                }
            );
        } else if name == "application" {
            let payload: Value =
                serde_json::from_slice(&result.stdout).expect("declared CLI error");
            assert_eq!(payload["ok"], false);
            assert_eq!(payload["error"]["kind"], "application");
            assert_eq!(payload["error"]["data"], json!({"code": "conflict"}));
        }
        assert_eq!(calls(&temp, name), 1, "CLI must reach handler: {name}");
    }

    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "tools.harn"]);
    let mut client = StdioJsonRpcClient::spawn("typed outcome MCP", command);
    let listing = client.request(request(1, "tools/list", json!({})));
    let tools = listing["result"]["tools"]
        .as_array()
        .expect("nonempty tools/list");
    assert_eq!(tools.len(), 8, "the real registry must be published");
    let ok = tools
        .iter()
        .find(|tool| tool["name"] == "ok")
        .expect("published explicit tool");
    assert_eq!(ok["outputSchema"]["required"], json!(["label"]));
    assert_eq!(ok["outputSchema"]["additionalProperties"], false);
    for (offset, name) in [
        "ok",
        "invalid",
        "error",
        "rejected",
        "malformed",
        "raw",
        "nominal",
        "application",
    ]
    .into_iter()
    .enumerate()
    {
        let response = client.request(request(
            offset as u64 + 2,
            "tools/call",
            json!({"name": name, "arguments": {}}),
        ));
        assert!(response.get("error").is_none(), "MCP {name}: {response}");
        let result = &response["result"];
        let succeeds = matches!(name, "ok" | "raw" | "nominal");
        assert_eq!(result["isError"], !succeeds, "MCP {name}: {response}");
        if succeeds {
            assert_eq!(
                result["structuredContent"],
                if name == "ok" {
                    json!({"label": "value"})
                } else {
                    domain.clone()
                }
            );
            if name == "ok" {
                assert_eq!(
                    result["content"],
                    json!([{ "type": "text", "text": "Feedback for ok" }])
                );
            }
        } else {
            assert!(
                result.get("structuredContent").is_none(),
                "failure leaked success data: {response}"
            );
            if name == "invalid" {
                assert!(
                    result["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("output violates its declared schema"),
                    "schema validator did not fire: {response}"
                );
            } else if name == "application" {
                assert_eq!(
                    result["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"],
                    json!({"code": "conflict"})
                );
            }
        }
        assert_eq!(calls(&temp, name), 2, "MCP must reach handler: {name}");
    }
    client.shutdown_expect_success();
}

#[test]
fn exported_mcp_resolves_typed_payload_schema_and_preserves_feedback() {
    let temp = fixture();
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "--surface", "exports", "tools.harn"]);
    let mut client = StdioJsonRpcClient::spawn("typed exported MCP", command);
    let listing = client.request(request(1, "tools/list", json!({})));
    let tools = listing["result"]["tools"]
        .as_array()
        .expect("nonempty exported tools/list");
    assert_eq!(tools.len(), 3);
    let payload = tools
        .iter()
        .find(|tool| tool["name"] == "exported_payload")
        .expect("typed export");
    assert_eq!(payload["outputSchema"]["required"], json!(["label"]));
    assert!(payload["outputSchema"]["properties"]
        .get("schema")
        .is_none());
    for (offset, mode) in ["ok", "invalid", "error", "rejected", "malformed"]
        .into_iter()
        .enumerate()
    {
        let response = client.request(request(
            offset as u64 + 2,
            "tools/call",
            json!({"name": "exported_payload", "arguments": {"mode": mode}}),
        ));
        assert!(response.get("error").is_none(), "export {mode}: {response}");
        let result = &response["result"];
        assert_eq!(result["isError"], mode != "ok", "export {mode}: {response}");
        if mode == "ok" {
            assert_eq!(result["structuredContent"], json!({"label": "value"}));
            assert_eq!(
                result["content"],
                json!([{ "type": "text", "text": "Export feedback for ok" }])
            );
        } else {
            assert!(
                result.get("structuredContent").is_none(),
                "export failure leaked success data: {response}"
            );
            if mode == "invalid" {
                assert!(
                    result["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("output violates its declared schema"),
                    "export output validator did not fire: {response}"
                );
            }
        }
        assert_eq!(
            calls(&temp, &format!("export_{mode}")),
            1,
            "real exported handler: {mode}"
        );
    }
    let nominal = client.request(request(
        7,
        "tools/call",
        json!({"name": "exported_nominal", "arguments": {}}),
    ));
    assert_eq!(nominal["result"]["isError"], false, "{nominal}");
    assert_eq!(
        nominal["result"]["structuredContent"],
        json!({"ok": false, "success": false, "status": "error"})
    );
    assert_eq!(calls(&temp, "export_nominal"), 1);
    let domain = client.request(request(
        8,
        "tools/call",
        json!({"name": "exported_domain", "arguments": {}}),
    ));
    assert_eq!(domain["result"]["isError"], false, "{domain}");
    assert_eq!(
        domain["result"]["structuredContent"],
        json!({
            "schema": "ordinary.domain.v2", "outcome": "error", "text": "Domain text",
            "data": {"label": "value"}
        })
    );
    assert_eq!(calls(&temp, "export_domain"), 1);
    client.shutdown_expect_success();
}
