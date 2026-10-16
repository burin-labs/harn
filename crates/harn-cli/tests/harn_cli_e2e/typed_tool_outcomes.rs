use std::fs;

use serde_json::{json, Value};

use crate::test_util::process::{harn_e2e_command, ChildGuard};
use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;

mod provenance {
    use super::*;

    enum Expected {
        Application(Value),
        Runtime,
        Contract,
    }

    pub(super) fn assert_cli_and_mcp_provenance() {
        let temp = tempfile::tempdir().unwrap();
        for (name, source) in [
            (
                "tools.harn",
                include_str!("../fixtures/typed_error_provenance.harn"),
            ),
            (
                "typed_error_dependency.harn",
                include_str!("../fixtures/typed_error_dependency.harn"),
            ),
        ] {
            fs::write(temp.path().join(name), source).unwrap();
        }
        let cases = [
            ("direct", Expected::Application(json!("application-probe"))),
            (
                "imported",
                Expected::Application(json!("application-probe")),
            ),
            (
                "imported_pipeline",
                Expected::Application(json!("application-probe")),
            ),
            (
                "declared_closure",
                Expected::Application(json!("application-probe")),
            ),
            ("dependency", Expected::Runtime),
            (
                "direct_finally",
                Expected::Application(json!("application-probe")),
            ),
            ("dependency_finally", Expected::Runtime),
            (
                "direct_try_star",
                Expected::Application(json!("application-probe")),
            ),
            ("dependency_try_star", Expected::Runtime),
            (
                "direct_retry",
                Expected::Application(json!("application-probe")),
            ),
            ("dependency_retry", Expected::Runtime),
            (
                "explicit_catch",
                Expected::Application(json!("application-probe")),
            ),
            ("scalar_finally", Expected::Application(json!(7))),
            ("undeclared_catch", Expected::Runtime),
            ("wrong_shape", Expected::Contract),
        ];
        for (name, expected) in &cases {
            let output = harn_e2e_command()
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
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            match expected {
                Expected::Application(data) => {
                    let value: Value =
                        serde_json::from_slice(&output.stdout).expect("application JSON");
                    assert_eq!(
                        value,
                        json!({"ok": false, "error": {"kind": "application", "tool": name, "data": data}}),
                        "{name}"
                    );
                }
                Expected::Runtime => {
                    assert!(output.stdout.is_empty(), "{name}: {output:?}");
                    assert!(
                        stderr.contains("tool threw an undeclared value"),
                        "{name}: {stderr}"
                    );
                }
                Expected::Contract => {
                    assert!(output.stdout.is_empty(), "{name}: {output:?}");
                    assert!(
                        stderr.contains("application error violates its declared schema"),
                        "{name}: {stderr}"
                    );
                }
            }
            assert!(!stderr.contains("private-runtime-detail-7782"), "{stderr}");
            assert!(!stderr.contains("private-contract-detail-7782"), "{stderr}");
            assert_eq!(
                fs::read_to_string(temp.path().join(format!("{name}.calls"))).unwrap(),
                "called\n"
            );
        }

        let mut command = harn_e2e_command();
        command.current_dir(temp.path()).args([
            "serve",
            "mcp",
            "--surface",
            "script",
            "tools.harn",
        ]);
        let mut client = StdioJsonRpcClient::spawn("exception provenance MCP", command);
        for (index, (name, expected)) in cases.iter().enumerate() {
            let response = client.request(super::request(
                index as u64 + 1,
                "tools/call",
                json!({"name": name, "arguments": {}}),
            ));
            let result = &response["result"];
            assert_eq!(result["isError"], true, "{name}: {response}");
            assert!(
                result.get("structuredContent").is_none(),
                "{name}: {response}"
            );
            let application = &result["_meta"]["com.harnlang/toolContract"]["applicationError"];
            match expected {
                Expected::Application(data) => assert_eq!(
                    application,
                    &json!({"tool": name, "data": data}),
                    "{name}: {response}"
                ),
                Expected::Runtime | Expected::Contract => {
                    assert!(application.is_null(), "{name}: {response}");
                    let text = result["content"][0]["text"]
                        .as_str()
                        .expect("redacted failure text");
                    let expected = if matches!(expected, Expected::Runtime) {
                        "tool threw an undeclared value"
                    } else {
                        "application error violates its declared schema"
                    };
                    assert!(text.contains(expected), "{name}: {response}");
                }
            }
            let serialized = response.to_string();
            assert!(!serialized.contains("private-runtime-detail-7782"));
            assert!(!serialized.contains("private-contract-detail-7782"));
            assert_eq!(
                fs::read_to_string(temp.path().join(format!("{name}.calls"))).unwrap(),
                "called\ncalled\n"
            );
        }
        client.shutdown_expect_success();
    }
}

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
    if mode == "invalid" || mode == "invalid_error" { {wrong: true} } else { {label: "value"} },
    if mode == "error" || mode == "invalid_error" { "error" } else if mode == "rejected" { "rejected" } else { "ok" },
  )
}

pub fn exported_payload(harness: Harness, mode: string) -> AgentToolHandlerResult<Widget> throws Widget {
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
  for name in ["ok", "invalid", "error", "rejected", "malformed", "declared_error", "invalid_error"] {
    let spec = {
      name: name, description: "Exercise one explicit tool result.",
      parameters: {}, returns: payload_schema,
      handler: { _ ->
        record_call(harness, name)
        if name == "malformed" {
          {schema: "harn.agent_tool_handler_result.v2", outcome: "ok", data: {label: "value"}}
        } else {
          agent_tool_handler_result(
            "Feedback for " + name,
            if name == "invalid" || name == "invalid_error" { {wrong: true} } else { {label: "value"} },
            if name == "error" || name == "declared_error" || name == "invalid_error" {
              "error"
            } else if name == "rejected" { "rejected" } else { "ok" },
          )
        }
      },
    }
    if name == "declared_error" || name == "invalid_error" { spec.error_schema = payload_schema }
    specs = specs + [spec]
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
"#;

const AGENT_TEST: &str = r#"
@test
pipeline agent_payload_contract(harness: Harness) {
  const direct = registry(harness)
  const owned = agent_lifecycle_tools(harness.agent, direct)
  for tools in [direct, owned] {
    for name in ["ok", "invalid", "error", "rejected", "malformed", "declared_error", "invalid_error"] {
      const result = agent_dispatch_tool_call(harness.tools, {name: name, arguments: {}}, tools)
      if name == "ok" {
        assert(result.ok)
        assert(result.result.schema == "harn.agent_tool_handler_result.v2")
        assert(result.result.data == {label: "value"})
        assert(result.rendered_result == "Feedback for ok")
      } else {
        assert(!result.ok, name + " must fail before agent success")
        assert(result.error_category == if name == "error" || name == "declared_error" {
          "tool_error"
        } else if name == "rejected" { "tool_rejected" } else { "schema_validation" })
      }
    }
  }
  const legacy = tool_registry_from([
    {name: "legacy_text", description: "Return rendered text", parameters: {}, returns: {type: "object"}, handler: {_ ->
      record_call(harness, "legacy_text")
      "Custom feedback"
    }},
    {name: "legacy_json", description: "Return rendered JSON", parameters: {}, returns: {type: "object"}, handler: {_ ->
      record_call(harness, "legacy_json")
      "{\"ok\":false}"
    }},
  ])
  for tools in [legacy, agent_lifecycle_tools(harness.agent, legacy)] {
    for name in ["legacy_text", "legacy_json"] {
      const result = agent_dispatch_tool_call(harness.tools, {name: name, arguments: {}}, tools)
      assert(result.ok)
      assert(result.result == if name == "legacy_text" { "Custom feedback" } else { "{\"ok\":false}" })
      assert(result.rendered_result == result.result)
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
    fs::write(
        temp.path().join("tools.harn"),
        format!("{FIXTURE}{AGENT_TEST}"),
    )
    .expect("add the agent-only test entrypoint");
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
    for name in [
        "ok",
        "invalid",
        "error",
        "rejected",
        "malformed",
        "declared_error",
        "invalid_error",
        "legacy_text",
        "legacy_json",
    ] {
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
    provenance::assert_cli_and_mcp_provenance();
    let temp = fixture();
    let domain = json!({"ok": false, "success": false, "status": "error"});
    for name in [
        "ok",
        "invalid",
        "error",
        "rejected",
        "malformed",
        "declared_error",
        "invalid_error",
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
        } else if matches!(
            name,
            "application" | "error" | "rejected" | "declared_error"
        ) {
            let payload: Value =
                serde_json::from_slice(&result.stdout).expect("declared CLI error");
            assert_eq!(payload["ok"], false);
            assert_eq!(payload["error"]["kind"], "application");
            assert_eq!(
                payload["error"]["data"],
                if name == "application" {
                    json!({"code": "conflict"})
                } else {
                    json!({"label": "value"})
                }
            );
            if name != "application" {
                assert_eq!(
                    payload["error"]["outcome"],
                    if name == "rejected" {
                        "rejected"
                    } else {
                        "error"
                    }
                );
            }
        }
        assert_eq!(calls(&temp, name), 1, "CLI must reach handler: {name}");
    }

    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "--surface", "script", "tools.harn"]);
    let mut client = StdioJsonRpcClient::spawn("typed outcome MCP", command);
    let listing = client.request(request(1, "tools/list", json!({})));
    let tools = listing["result"]["tools"]
        .as_array()
        .expect("nonempty tools/list");
    assert_eq!(tools.len(), 10, "the real registry must be published");
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
        "declared_error",
        "invalid_error",
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
            } else if matches!(name, "error" | "rejected" | "declared_error") {
                let failure = &result["_meta"]["com.harnlang/toolContract"]["applicationError"];
                assert_eq!(failure["data"], json!({"label": "value"}));
                assert_eq!(
                    failure["outcome"],
                    if name == "rejected" {
                        "rejected"
                    } else {
                        "error"
                    }
                );
            } else if name == "invalid_error" || name == "malformed" {
                assert!(result["_meta"]["com.harnlang/toolContract"]
                    .get("applicationError")
                    .is_none());
            }
        }
        assert_eq!(calls(&temp, name), 2, "MCP must reach handler: {name}");
    }
    client.shutdown_expect_success();
}

#[tokio::test]
async fn exported_mcp_resolves_typed_payload_schema_and_preserves_feedback() {
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
    assert_eq!(
        payload["_meta"]["com.harnlang/toolContract"]["errorSchema"]["required"],
        json!(["label"])
    );
    assert!(payload["outputSchema"]["properties"]
        .get("schema")
        .is_none());
    for (offset, mode) in [
        "ok",
        "invalid",
        "error",
        "rejected",
        "malformed",
        "invalid_error",
    ]
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
            } else if mode == "error" || mode == "rejected" {
                let failure = &result["_meta"]["com.harnlang/toolContract"]["applicationError"];
                assert_eq!(failure["data"], json!({"label": "value"}));
                assert_eq!(failure["outcome"], mode);
            } else {
                assert!(result["_meta"]["com.harnlang/toolContract"]
                    .get("applicationError")
                    .is_none());
                if mode == "invalid_error" {
                    assert!(result["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("application error violates its declared schema"));
                }
            }
        }
        assert_eq!(
            calls(&temp, &format!("export_{mode}")),
            1,
            "real exported handler: {mode}"
        );
    }
    let nominal = client.request(request(
        8,
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
        9,
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
    fs::write(
        temp.path().join("site.harn"),
        r#"
import { AgentToolHandlerResult } from "std/agent/tool_lifecycle"
import { exported_payload } from "./tools"

type Widget = {label: string}

@route("GET", "/typed/{mode}")
pub fn typed(harness: Harness, req: dict) -> AgentToolHandlerResult<Widget> throws Widget {
  return exported_payload(harness, req.path_params.mode)
}
"#,
    )
    .expect("write actual HTTP projection");
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "site", "--bind", "127.0.0.1:0", "site.harn"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = ChildGuard(command.spawn().expect("start actual site adapter"));
    let (rx, _stderr) =
        crate::test_util::stdio_jsonrpc::spawn_line_reader(child.0.stderr.take().unwrap());
    let url = crate::test_util::stdio_jsonrpc::wait_for_child_log_suffix(
        &mut child.0,
        &rx,
        "Site server ready on ",
        std::time::Duration::from_mins(1),
        "typed outcome site",
    );
    let http = reqwest::Client::new();
    for mode in [
        "ok",
        "error",
        "rejected",
        "invalid",
        "invalid_error",
        "malformed",
    ] {
        let response = http
            .get(format!("{url}/typed/{mode}"))
            .send()
            .await
            .expect("actual HTTP request");
        let status = response.status().as_u16();
        let payload: Value = response.json().await.expect("HTTP JSON response");
        if mode == "ok" {
            assert_eq!(status, 200, "{payload}");
            assert_eq!(payload, json!({"label": "value"}));
        } else if mode == "error" || mode == "rejected" {
            assert_eq!(status, 422, "{payload}");
            assert_eq!(payload["code"], "application_error");
            assert_eq!(payload["details"]["data"], json!({"label": "value"}));
            assert_eq!(payload["details"]["outcome"], mode);
        } else {
            assert_eq!(status, 500, "{payload}");
            assert!(
                payload["details"].get("data").is_none(),
                "invalid failure leaked application data: {payload}"
            );
        }
        assert_eq!(
            calls(&temp, &format!("export_{mode}")),
            2,
            "actual HTTP handler: {mode}"
        );
    }
}
