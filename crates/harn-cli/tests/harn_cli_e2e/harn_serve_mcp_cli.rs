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

mod publication;
use publication::write_export_fixture;

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

pub(super) fn stable_request(id: u64, method: &str, mut params: JsonValue) -> JsonValue {
    params["_meta"] = stable_meta(json!({
        "elicitation": {"form": {}, "url": {}},
        "roots": {},
        "sampling": {},
    }));
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[test]
fn serve_mcp_code_mode_dispatches_only_server_registered_read_tools() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("server.harn"),
        r##"
import { composition_mcp_tools } from "std/composition"
import { agent_dispatch_tool_call } from "std/agent/primitives"
import { agent_tool_handler_result } from "std/agent/tool_lifecycle"

fn main(harness: Harness) {
  let source = tool_registry({name: "composition-fixture"}, {schemas: {MessageValue: {type: "string"}}})
  source = tool_define(source, "echo", "Echo a value", {
    parameters: {value: {type: "string", required: true}},
    annotations: {side_effect_level: "read_only"},
    handler: {args -> "bound:${args.value}"},
  })
  source = tool_define(source, "other", "Read another value", {
    parameters: {},
    annotations: {side_effect_level: "read_only"},
    handler: {_args -> "other dispatched"},
  })
  source = tool_define(source, "agent_only", "An agent-only read", {
    parameters: {},
    annotations: {side_effect_level: "read_only"},
    governance: {audiences: ["agent"]},
    handler: {_args -> "agent-only handler reached"},
  })
  source = tool_define(source, "mcp_only", "An MCP-only read", {
    input_schema: {type: "object", properties: {value: {"$ref": "#/components/schemas/MessageValue"}}, required: ["value"]},
    annotations: {side_effect_level: "read_only"},
    governance: {audiences: ["mcp"]},
    prepare: {args -> {operation: {value: args.value}}},
    handler: {_args -> "mcp-only:${tool_invocation_binding().operation.value}"},
  })
  const before_forgery_tool = source
  source = tool_define(source, "forge_audience", "Try to forge composition authority", {
    parameters: {},
    returns: {type: "object"},
    annotations: {side_effect_level: "read_only"},
    handler: {_args ->
      const hidden = tool_find(before_forgery_tool, "agent_only")
      if hidden == nil { throw "agent-only fixture missing" }
      const forged_registry = {_type: "tool_registry", tools: [hidden + {governance: {audiences: ["mcp"]}}]}
      const attempt = agent_dispatch_tool_call(harness.tools, {name: "agent_only", arguments: {}}, forged_registry, {_composition_mcp: true})
      const visible = tool_find(before_forgery_tool, "mcp_only")
      if visible == nil { throw "MCP-only fixture missing" }
      const alias = before_forgery_tool + {tools: [visible + {handler: hidden.handler}]}
      const alias_attempt = agent_dispatch_tool_call(harness.tools, {name: "mcp_only", arguments: {value: "forged"}}, alias, {_composition_mcp: true})
      const altered_schema = before_forgery_tool + {components: {schemas: {MessageValue: {type: "integer"}}}}
      const schema_attempt = agent_dispatch_tool_call(harness.tools, {name: "mcp_only", arguments: {value: 7}}, altered_schema, {_composition_mcp: true})
      const schema_valid = agent_dispatch_tool_call(harness.tools, {name: "mcp_only", arguments: {value: "published"}}, altered_schema, {_composition_mcp: true})
      const result = {allowed: attempt.ok, alias_allowed: alias_attempt.ok, schema_allowed: schema_attempt.ok, schema_error_category: schema_attempt.error_category, schema_valid: schema_valid.ok}
      return agent_tool_handler_result(to_string(result), result)
    },
  })
  source = tool_define(source, "mutate", "A write operation", {
    parameters: {},
    annotations: {side_effect_level: "workspace_write"},
    handler: {_args -> "write was dispatched"},
  })
  source = tool_define(source, "unknown_effect", "Undeclared side effects", {
    parameters: {},
    handler: {_args -> "unknown tool was dispatched"},
  })
  const ordinary_ok = agent_dispatch_tool_call(harness.tools, {name: "echo", arguments: {value: "direct"}}, source)
  assert(ordinary_ok.ok)
  const ordinary_invalid = agent_dispatch_tool_call(harness.tools, {name: "echo", arguments: {value: 7}}, source)
  assert(!ordinary_invalid.ok)
  assert(ordinary_invalid.error_category == "schema_validation")
  const agent_only = agent_dispatch_tool_call(harness.tools, {name: "agent_only", arguments: {}}, source)
  assert(agent_only.ok)
  const forged = agent_dispatch_tool_call(harness.tools, {name: "mcp_only", arguments: {value: "forged"}}, source, {_composition_mcp: true})
  assert(!forged.ok)
  harness.tools.mcp_tools(composition_mcp_tools(harness, source))
}
"##,
    )
    .unwrap();
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "server.harn"]);
    let mut client = StdioJsonRpcClient::spawn("Code Mode MCP server", command);
    let discovered = client.request(stable_request(0, "tools/list", json!({})));
    let tools = discovered["result"]["tools"].as_array().expect("tool list");
    for name in [
        "echo",
        "harn.code.generate_harn_api",
        "harn.code.execute_composition",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "{discovered}"
        );
    }
    assert!(
        !tools.iter().any(|tool| tool["name"] == "phantom"),
        "{discovered}"
    );
    assert!(
        !tools.iter().any(|tool| tool["name"] == "agent_only"),
        "{discovered}"
    );
    let hidden = client.request(stable_request(
        14,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return agent_only({})",
            "manifest": {"bindings": [{"name": "agent_only", "binding": "agent_only"}]}
        }}),
    ));
    assert_eq!(hidden["result"]["isError"], true, "{hidden}");
    let forged = client.request(stable_request(
        17,
        "tools/call",
        json!({"name": "forge_audience", "arguments": {}}),
    ));
    assert_eq!(
        forged["result"]["structuredContent"]["allowed"], false,
        "{forged}"
    );
    assert_eq!(
        forged["result"]["structuredContent"]["alias_allowed"], false,
        "{forged}"
    );
    let nested_forgery = client.request(stable_request(
        18,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return forge_audience({})"
        }}),
    ));
    assert_eq!(
        nested_forgery["result"]["structuredContent"]["result"]["data"]["allowed"], false,
        "{nested_forgery}"
    );
    assert_eq!(
        nested_forgery["result"]["structuredContent"]["result"]["data"]["alias_allowed"], false,
        "{nested_forgery}"
    );
    assert_eq!(
        nested_forgery["result"]["structuredContent"]["result"]["data"]["schema_allowed"], false,
        "{nested_forgery}"
    );
    assert_eq!(
        nested_forgery["result"]["structuredContent"]["result"]["data"]["schema_error_category"],
        "schema_validation",
        "{nested_forgery}"
    );
    assert_eq!(
        nested_forgery["result"]["structuredContent"]["result"]["data"]["schema_valid"], true,
        "{nested_forgery}"
    );
    let mcp_only = client.request(stable_request(
        15,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return mcp_only({value: \"ok\"})"
        }}),
    ));
    assert_ne!(mcp_only["result"]["isError"], true, "{mcp_only}");
    assert_eq!(
        mcp_only["result"]["structuredContent"]["result"], "mcp-only:ok",
        "{mcp_only}"
    );
    let invalid_mcp_only = client.request(stable_request(
        16,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return mcp_only({value: 7})", "include_report": true
        }}),
    ));
    assert_eq!(
        invalid_mcp_only["result"]["isError"], true,
        "{invalid_mcp_only}"
    );
    assert_eq!(
        invalid_mcp_only["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]
            ["data"]["child_results"][0]["error_category"],
        "schema_validation",
        "{invalid_mcp_only}"
    );
    let api = client.request(stable_request(
        1,
        "tools/call",
        json!({"name": "harn.code.generate_harn_api", "arguments": {}}),
    ));
    let mut manifest = api["result"]["structuredContent"]["manifest"].clone();
    assert_eq!(manifest["bindings"].as_array().unwrap().len(), 4, "{api}");
    assert_eq!(manifest["bindings"][0]["name"], "echo");
    manifest["bindings"][0]["metadata"]["mock_output"] = json!("fabricated");
    let called = client.request(stable_request(
        2,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return echo({value: \"hello\"})",
            "manifest": manifest,
            "include_report": true
        }}),
    ));
    assert_ne!(called["result"]["isError"], true, "{called}");
    let report = &called["result"]["structuredContent"];
    assert_eq!(report["run"]["result"], "bound:hello", "{called}");
    assert_eq!(report["child_calls"][0]["tool_name"], "echo", "{called}");

    let invalid = client.request(stable_request(
        10,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return echo({})", "include_report": true
        }}),
    ));
    assert_eq!(invalid["result"]["isError"], true, "{invalid}");
    let failure =
        &invalid["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"];
    assert_eq!(failure["child_results"][0]["status"], "failed", "{invalid}");
    assert_eq!(
        failure["child_results"][0]["raw_output"],
        JsonValue::Null,
        "{invalid}"
    );

    let invalid_direct = client.request(stable_request(
        11,
        "tools/call",
        json!({"name": "echo", "arguments": {"value": 7}}),
    ));
    assert_eq!(invalid_direct["error"]["code"], -32602, "{invalid_direct}");
    let invalid_child = client.request(stable_request(
        12,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return echo({value: 7})", "include_report": true
        }}),
    ));
    assert_eq!(invalid_child["result"]["isError"], true, "{invalid_child}");
    let failure =
        &invalid_child["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"];
    assert_eq!(
        failure["child_results"][0]["status"], "failed",
        "{invalid_child}"
    );
    assert_eq!(
        failure["child_results"][0]["raw_output"],
        JsonValue::Null,
        "{invalid_child}"
    );
    assert_eq!(
        failure["child_results"][0]["error_category"], "schema_validation",
        "{invalid_child}"
    );
    assert!(
        failure["child_results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("declared schema"),
        "{invalid_child}"
    );

    for (id, name) in [
        (3, "phantom"),
        (4, "mutate"),
        (5, "unknown_effect"),
        (13, "agent_only"),
    ] {
        let denied = client.request(stable_request(
            id,
            "tools/call",
            json!({"name": "harn.code.execute_composition", "arguments": {
                "snippet": format!("return {name}({{}})"),
                "manifest": {"bindings": [{"name": name, "binding": name,
                    "metadata": {"mock_output": "fabricated"}}]}
            }}),
        ));
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert_eq!(
            denied["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"]
                ["error"],
            "composition_mcp: manifest contains an unregistered binding"
        );
    }
    let searched = client.request(stable_request(
        6,
        "tools/call",
        json!({"name": "harn.code.generate_harn_api", "arguments": {"query": "echo", "limit": 1}}),
    ));
    assert_eq!(
        searched["result"]["structuredContent"]["tool_count"], 1,
        "{searched}"
    );
    assert_eq!(
        searched["result"]["structuredContent"]["manifest"]["bindings"][0]["name"],
        "echo"
    );
    let narrowed = client.request(stable_request(
        9,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return other({})",
            "manifest": searched["result"]["structuredContent"]["manifest"]
        }}),
    ));
    assert_eq!(narrowed["result"]["isError"], true, "{narrowed}");
    let supplied_tools = client.request(stable_request(
        7,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return phantom({})", "tools": [{"name": "phantom"}]
        }}),
    ));
    assert_eq!(
        supplied_tools["result"]["isError"], true,
        "{supplied_tools}"
    );
    assert_eq!(
        supplied_tools["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"]
            ["error"],
        "composition_mcp: tools are configured by the server"
    );
    let supplied_trust = client.request(stable_request(
        8,
        "tools/call",
        json!({"name": "harn.code.execute_composition", "arguments": {
            "snippet": "return echo({value: \"hello\"})", "trust_annotations": true
        }}),
    ));
    assert_eq!(
        supplied_trust["result"]["isError"], true,
        "{supplied_trust}"
    );
    assert_eq!(
        supplied_trust["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"]["data"]
            ["error"],
        "composition_mcp: trust is configured by the server"
    );
    client.shutdown_expect_success();
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
      returns: {
        type: "object",
        properties: {message: {type: "string"}},
        required: ["message"],
        additionalProperties: false,
      },
      cli: {
        command: ["people", "greet"],
        arguments: {name: {position: 0, value_name: "NAME"}},
      },
      handler: {args -> {message: "Hello, " + args.name}},
    },
  ], {
    info: {name: "widgets", version: "1.2.3", description: "Widget integration"},
    cli: {commands: [{command: ["people"], title: "Work with people"}]},
  })
  harness.tools.mcp_tools(tools)
}
"#,
    )
    .unwrap();
}

fn write_governed_registry_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
import { tool_registry_from } from "std/tools"

fn main(harness: Harness) {
  const tools = tool_registry_from([
    {
      name: "operator_receipt",
      description: "Read one operator receipt.",
      parameters: {},
      governance: {audiences: ["catalog", "cli"]},
      handler: {_args -> {surface: "cli"}},
    },
    {
      name: "remote_probe",
      description: "Probe one remote integration.",
      parameters: {},
      governance: {audiences: ["catalog", "mcp"]},
      handler: {_args -> {surface: "mcp"}},
    },
  ], {info: {name: "governed-tools"}})
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
  ], {{info: {{name: "reload-fixture"}}}})
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
    input_schema: {
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
fn registry_handler_has_generated_cli_and_mcp_parity() {
    let temp = TempDir::new().unwrap();
    write_registry_info_fixture(&temp);
    let script = temp.path().join("server.harn");
    let cli = harn_e2e_command()
        .args([
            "tool",
            "run",
            &script.display().to_string(),
            "people",
            "greet",
            "Harn",
            "--json",
        ])
        .output()
        .expect("generated CLI invocation");
    assert!(
        cli.status.success(),
        "generated CLI failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_result: JsonValue = serde_json::from_slice(&cli.stdout).expect("generated CLI JSON");

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
        json!({"name": "greet", "arguments": {"name": "Harn"}}),
    ));
    assert_eq!(called["result"]["resultType"], "complete");
    assert_eq!(
        called["result"]["structuredContent"]["message"],
        "Hello, Harn"
    );
    assert_eq!(called["result"]["structuredContent"], cli_result);
    client.shutdown_expect_success();
}

fn write_handler_envelope_registry_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("widget_tools.harn"),
        r#"
import { tool_registry_from, ToolRegistry } from "std/tools"
import { agent_tool_handler_result } from "std/agent/tool_lifecycle"

const WIDGET = {
  type: "object",
  properties: {id: {type: "integer"}, name: {type: "string"}},
  required: ["id", "name"],
  additionalProperties: false,
}

fn get_widget(args: dict) -> dict {
  let r = {id: args.widget_id, name: "sprocket"}
  return agent_tool_handler_result(json_stringify(r), r)
}

fn invalid_widget(args: dict) -> dict {
  let r = {id: "not-an-integer", name: "sprocket"}
  return agent_tool_handler_result(json_stringify(r), r)
}

fn fail_widget(args: dict) -> dict {
  return agent_tool_handler_result("widget lookup failed", {code: "not_found"}, "error")
}

fn raw_widget(args: dict) -> dict {
  return {id: args.widget_id, name: "raw"}
}

pub fn widget_tools() -> ToolRegistry {
  return tool_registry_from([
    {
      name: "get",
      description: "Widget tool get.",
      parameters: {widget_id: {schema: {type: "integer"}, required: true}},
      returns: WIDGET,
      cli: {command: ["widgets", "get"]},
      handler: get_widget,
    },
    {
      name: "invalid",
      description: "Widget tool invalid.",
      parameters: {widget_id: {schema: {type: "integer"}, required: true}},
      returns: WIDGET,
      cli: {command: ["widgets", "invalid"]},
      handler: invalid_widget,
    },
    {
      name: "fail",
      description: "Widget tool fail.",
      parameters: {widget_id: {schema: {type: "integer"}, required: true}},
      returns: WIDGET,
      cli: {command: ["widgets", "fail"]},
      handler: fail_widget,
    },
    {
      name: "raw",
      description: "Widget tool raw.",
      parameters: {widget_id: {schema: {type: "integer"}, required: true}},
      returns: WIDGET,
      cli: {command: ["widgets", "raw"]},
      handler: raw_widget,
    },
  ], {info: {name: "widgets", version: "1.0.0"}})
}

"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("agent.harn"),
        r#"
import { widget_tools } from "./widget_tools"
import { agent_dispatch_tool_call } from "std/agent/primitives"

@test
pipeline agent_schema_parity(harness: Harness) {
  const tools = widget_tools()
  const ok = agent_dispatch_tool_call(harness.tools, {name: "get", arguments: {widget_id: 7}}, tools)
  assert(ok.ok)
  assert(ok.result.data == {id: 7, name: "sprocket"})
  const invalid = agent_dispatch_tool_call(harness.tools, {name: "invalid", arguments: {widget_id: 7}}, tools)
  assert(!invalid.ok)
  assert(invalid.error_category == "schema_validation")
  assert(invalid.error.contains("output violates its declared schema"))
  const failed = agent_dispatch_tool_call(harness.tools, {name: "fail", arguments: {widget_id: 7}}, tools)
  assert(!failed.ok)
  assert(failed.error_category == "tool_error")
}
"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("server.harn"),
        r#"
import { widget_tools } from "./widget_tools"

fn main(harness: Harness) {
  harness.tools.mcp_tools(widget_tools())
}
"#,
    )
    .unwrap();
}

/// A handler written to the agent-dispatch result envelope must mean the same
/// thing on agent dispatch, the generated CLI, and MCP: the declared output
/// schema validates `data`, and an `error` outcome is a failure.
#[test]
fn registry_handler_result_envelope_has_cli_and_mcp_parity() {
    let temp = TempDir::new().unwrap();
    write_handler_envelope_registry_fixture(&temp);
    let script = temp.path().join("server.harn").display().to_string();
    let agent_script = temp.path().join("agent.harn").display().to_string();
    let agent = harn_e2e_command()
        .current_dir(temp.path())
        .args(["test", &agent_script])
        .output()
        .expect("agent dispatcher invocation");
    assert!(
        agent.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&agent.stdout),
        String::from_utf8_lossy(&agent.stderr)
    );
    assert!(
        String::from_utf8_lossy(&agent.stdout).contains("1 passed"),
        "agent parity case must execute"
    );
    let cli = |command: &str| {
        harn_e2e_command()
            .args([
                "tool",
                "run",
                &script,
                "widgets",
                command,
                "--widget-id",
                "7",
                "--json",
            ])
            .output()
            .expect("generated CLI invocation")
    };

    let ok = cli("get");
    assert!(
        ok.status.success(),
        "ok envelope failed on the CLI: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let ok_json: JsonValue = serde_json::from_slice(&ok.stdout).expect("CLI JSON");
    assert_eq!(ok_json, json!({"id": 7, "name": "sprocket"}));

    let raw = cli("raw");
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<JsonValue>(&raw.stdout).unwrap(),
        json!({"id": 7, "name": "raw"})
    );

    let invalid = cli("invalid");
    assert!(!invalid.status.success(), "schema-violating data succeeded");
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        stderr.contains("output violates its declared schema"),
        "{stderr}"
    );
    // Validating the envelope itself would report missing `id`/`name`;
    // validating `data` reports the wrongly typed field.
    assert!(stderr.contains("failed type"), "{stderr}");
    assert!(
        !stderr.contains("required"),
        "envelope was validated: {stderr}"
    );

    let failed = cli("fail");
    assert!(
        !failed.status.success(),
        "error outcome succeeded on the CLI"
    );
    assert_eq!(
        serde_json::from_slice::<JsonValue>(&failed.stdout).expect("CLI error envelope"),
        json!({
            "ok": false,
            "error": {
                "kind": "application",
                "tool": "fail",
                "data": {"code": "not_found"},
                "outcome": "error",
            },
        })
    );

    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "server.harn"]);
    let mut client = StdioJsonRpcClient::spawn("harn serve mcp", command);
    let call = |client: &mut StdioJsonRpcClient, id: u64, name: &str| {
        client.request(stable_request(
            id,
            "tools/call",
            json!({"name": name, "arguments": {"widget_id": 7}}),
        ))
    };

    let ok = call(&mut client, 1, "get");
    assert_eq!(ok["result"]["isError"], false, "{ok}");
    assert_eq!(ok["result"]["structuredContent"], ok_json, "{ok}");

    let invalid = call(&mut client, 2, "invalid");
    assert_eq!(invalid["result"]["isError"], true, "{invalid}");
    assert!(
        invalid["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("output violates its declared schema")),
        "{invalid}"
    );

    let failed = call(&mut client, 3, "fail");
    assert_eq!(failed["result"]["isError"], true, "{failed}");
    assert!(
        failed["result"].get("structuredContent").is_none(),
        "{failed}"
    );
    assert_eq!(
        failed["result"]["_meta"]["com.harnlang/toolContract"]["applicationError"],
        json!({"tool": "fail", "data": {"code": "not_found"}, "outcome": "error"}),
        "{failed}"
    );
    client.shutdown_expect_success();
}

#[ignore = "binary surface: runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_excludes_and_denies_tools_without_the_mcp_audience() {
    let temp = TempDir::new().unwrap();
    write_governed_registry_fixture(&temp);
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("governed harn serve mcp", command);

    let listed = client.request(stable_request(1, "tools/list", json!({})));
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["remote_probe"]);

    let denied = client.request(stable_request(
        2,
        "tools/call",
        json!({"name": "operator_receipt", "arguments": {}}),
    ));
    assert_eq!(denied["error"]["code"], -32602);
    assert_eq!(denied["error"]["message"], "Unknown tool: operator_receipt");
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

fn write_component_backed_registry_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r##"
import { tool_registry_from } from "std/tools"

fn main(harness: Harness) {
  const tools = tool_registry_from([
    {
      name: "lookup_widget",
      description: "Look up one widget.",
      input_schema: {
        type: "object",
        "$defs": {"Widget/Id~": {type: "integer", minimum: 1}},
        properties: {widget_id: {"$ref": "#/$defs/Widget~1Id~0", description: "Identifier from a local schema"}},
        required: ["widget_id"],
        additionalProperties: false,
      },
      returns: {"$ref": "#/components/schemas/Widget"},
      cli: {
        command: ["widgets", "get"],
        arguments: {widget_id: {position: 0, value_name: "WIDGET"}},
      },
      handler: {args -> {id: args.widget_id}},
    },
    {
      name: "broken_widget",
      description: "Return one widget outside its declared shape.",
      input_schema: {
        type: "object",
        "$defs": {LocalWidgetId: {"$ref": "#/components/schemas/WidgetId"}},
        properties: {widget_id: {"$ref": "#/$defs/LocalWidgetId"}},
        required: ["widget_id"],
        additionalProperties: false,
      },
      returns: {"$ref": "#/components/schemas/Widget"},
      cli: {
        command: ["widgets", "broken"],
        arguments: {widget_id: {position: 0, value_name: "WIDGET"}},
      },
      handler: {_args -> {id: "not-an-integer"}},
    },
    {
      name: "echo_identifier",
      description: "Echo one local identifier.",
      input_schema: {
        type: "object",
        "$defs": {Identifier: {type: "string", pattern: "^[a-z]+$"}},
        properties: {id: {"$ref": "#/$defs/Identifier"}},
        required: ["id"], additionalProperties: false,
      },
      returns: {type: "string"},
      cli: {command: ["echo_identifier"], arguments: {id: {completions: ["ok", "other"]}}},
      handler: {args -> args.id},
    },
  ], {
    info: {name: "widgets", version: "1.0.0"},
    components: {
      schemas: {
        WidgetId: {type: "integer", minimum: 1},
        Widget: {
          type: "object",
          properties: {id: {type: "integer"}},
          required: ["id"],
          additionalProperties: false,
        },
      },
    },
  })
  harness.tools.mcp_tools(tools)
}
"##,
    )
    .unwrap();
}

/// A component-backed catalog must execute, not merely publish. Both surfaces
/// resolve the shared reference, reject the same out-of-range input before the
/// handler runs, and refuse to report an out-of-contract result as a success.
#[ignore = "binary surface: runs in the slow E2E/smoke job"]
#[test]
fn component_backed_registry_enforces_the_same_contract_on_cli_and_mcp() {
    let temp = TempDir::new().unwrap();
    write_component_backed_registry_fixture(&temp);
    let script = temp.path().join("server.harn");
    let script = script.display().to_string();

    let help = harn_e2e_command()
        .args(["tool", "run", &script, "widgets", "get", "--help"])
        .output()
        .expect("local-reference CLI help");
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("Identifier from a local schema"));
    let completions = harn_e2e_command()
        .args(["tool", "completions", &script, "--shell", "fish"])
        .output()
        .expect("local-reference completions");
    assert!(completions.status.success(), "{completions:?}");
    let candidates = String::from_utf8_lossy(&completions.stdout);
    assert!(candidates.contains("other"), "{candidates}");

    let cli_ok = harn_e2e_command()
        .args(["tool", "run", &script, "widgets", "get", "7", "--json"])
        .output()
        .expect("generated CLI invocation");
    assert!(
        cli_ok.status.success(),
        "component-backed CLI call failed: {}",
        String::from_utf8_lossy(&cli_ok.stderr)
    );
    let cli_result: JsonValue = serde_json::from_slice(&cli_ok.stdout).expect("CLI JSON");
    assert_eq!(cli_result, json!({"id": 7}));

    let cli_invalid_input = harn_e2e_command()
        .args(["tool", "run", &script, "widgets", "get", "0", "--json"])
        .output()
        .expect("invalid CLI input");
    assert!(!cli_invalid_input.status.success());
    let stderr = String::from_utf8_lossy(&cli_invalid_input.stderr);
    assert!(stderr.contains("minimum"), "{stderr}");

    let cli_invalid_output = harn_e2e_command()
        .args(["tool", "run", &script, "widgets", "broken", "7", "--json"])
        .output()
        .expect("invalid CLI output");
    assert!(
        !cli_invalid_output.status.success(),
        "an out-of-contract result must not be reported as a success"
    );

    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .arg("serve")
        .arg("mcp")
        .arg("server.harn");
    let mut client = StdioJsonRpcClient::spawn("component-backed harn serve mcp", command);

    let listed = client.request(stable_request(1, "tools/list", json!({})));
    let advertised = listed["result"]["tools"]
        .as_array()
        .expect("advertised tools")
        .iter()
        .find(|tool| tool["name"] == "lookup_widget")
        .expect("lookup_widget is advertised")
        .clone();
    let input_schema = serde_json::to_string(&advertised["inputSchema"]).expect("input schema");
    assert!(
        input_schema.contains("\"minimum\":1"),
        "MCP discovery must carry the shared component's constraint: {input_schema}"
    );
    assert!(
        !input_schema.contains("#/components/schemas/"),
        "MCP discovery must publish a self-contained schema: {input_schema}"
    );

    let called = client.request(stable_request(
        2,
        "tools/call",
        json!({"name": "lookup_widget", "arguments": {"widget_id": 7}}),
    ));
    assert_eq!(called["result"]["structuredContent"], cli_result);

    let invalid_input = client.request(stable_request(
        3,
        "tools/call",
        json!({"name": "lookup_widget", "arguments": {"widget_id": 0}}),
    ));
    assert_eq!(invalid_input["error"]["code"], -32602);
    assert!(
        invalid_input["error"]["message"]
            .as_str()
            .expect("validation message")
            .contains("minimum"),
        "{invalid_input}"
    );

    let invalid_output = client.request(stable_request(
        4,
        "tools/call",
        json!({"name": "broken_widget", "arguments": {"widget_id": 7}}),
    ));
    assert_eq!(
        invalid_output["result"]["isError"],
        json!(true),
        "{invalid_output}"
    );
    for (request_id, value, valid) in [(5, "ok", true), (6, "1", false)] {
        let cli = harn_e2e_command()
            .args([
                "tool",
                "run",
                &script,
                "echo_identifier",
                "--id",
                value,
                "--json",
            ])
            .output()
            .expect("local-reference string CLI input");
        assert_eq!(cli.status.success(), valid, "{cli:?}");
        let mcp = client.request(stable_request(
            request_id,
            "tools/call",
            json!({"name": "echo_identifier", "arguments": {"id": value}}),
        ));
        if valid {
            let result: JsonValue = serde_json::from_slice(&cli.stdout).unwrap();
            assert_eq!(result, "ok");
            assert_eq!(mcp["result"]["structuredContent"], json!({"result": "ok"}));
        } else {
            assert!(
                String::from_utf8_lossy(&cli.stderr).contains("pattern"),
                "{cli:?}"
            );
            assert_eq!(mcp["error"]["code"], -32602, "{mcp}");
            assert!(mcp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("pattern"));
        }
    }
    client.shutdown_expect_success();
}
